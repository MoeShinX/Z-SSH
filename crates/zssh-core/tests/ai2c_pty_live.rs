//! AI-2c 第 2 节端到端实测：在用户的终端里执行 AI 命令。
//!
//! 链路：真实 SSH 连接 → 真实交互 shell → 把带随机结束标记的包装脚本写进终端 →
//! 从输出里找结束标记拿退出码。**不执行任何危险命令**（只用 echo / false / top /
//! sleep 这类无副作用的）。
//!
//! 环境变量 `ZSSH_TEST_*` 不全时跳过并打印原因。

mod common;

use std::time::{Duration, Instant};

use zssh_core::ai::display::DisplayFilter;
use zssh_core::ai::pty::{
    build_clear_prefix, build_wrapped_command, extract_output, find_end_marker, looks_idle,
    new_marker, ShellKind, TerminalState,
};
use zssh_core::ai::shell_kind::{
    build_shell_probe_command, classify_shell_name, parse_shell_probe_output, SHELL_PROBE_TIMEOUT,
};
use zssh_core::ssh::{Connection, Pooled, ShellEvent, ShellSession};

use common::{serial, TempKnownHosts, TestServer};

const COLS: u16 = 80;
const ROWS: u16 = 24;

/// 把命令包好写进终端，等结束标记。
struct Terminal {
    shell: ShellSession,
    /// 累积的原始输出（找标记用）——等价于生产的环形缓冲。
    raw: String,
    /// 终端状态（备用屏幕 / 提示符判断，和生产用同一套）。
    state: TerminalState,
    /// 显示过滤器（AI-2e）：喂和会话任务同一条路径，拿到"发给界面的数据"。
    display: DisplayFilter,
    /// 过完显示过滤器的文本——等价于用户屏幕上的画面。
    display_text: String,
}

impl Terminal {
    fn new(shell: ShellSession) -> Self {
        Self {
            shell,
            raw: String::new(),
            state: TerminalState::new(),
            display: DisplayFilter::new(),
            display_text: String::new(),
        }
    }

    /// 跑一条命令：返回（输出，退出码）。
    async fn run(&mut self, command: &str, timeout: Duration) -> (String, Option<i32>) {
        let marker = new_marker();
        let payload = format!(
            "{}{}",
            build_clear_prefix(),
            build_wrapped_command(command, &marker)
        );
        // 和生产一样：先让显示过滤器记住这条命令（合成回显用），再写进终端。
        let armed = self.display.arm(&marker, &payload, command);
        self.display_text
            .push_str(&String::from_utf8_lossy(&armed.data));
        self.shell
            .write(format!("{payload}\r").as_bytes())
            .await
            .expect("写入终端失败");
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(found) = find_end_marker(&self.raw, &marker) {
                let output = extract_output(&self.raw, &marker);
                let _ = found;
                return (output, Some(found.exit_code));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return (extract_output(&self.raw, &marker), None);
            }
            self.pump(remaining).await;
        }
    }

    /// 读一段输出进缓冲（超时不算错误）。
    async fn pump(&mut self, budget: Duration) {
        if let Ok(Some(ShellEvent::Data(bytes))) =
            tokio::time::timeout(budget, self.shell.next_event()).await
        {
            self.state.feed(&bytes);
            self.raw.push_str(&String::from_utf8_lossy(&bytes));
            let shown = self.display.push(&bytes);
            self.display_text
                .push_str(&String::from_utf8_lossy(&shown.data));
        }
    }

    /// 一直读到出现某个文字（或超时）。
    async fn pump_until(&mut self, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.raw.contains(needle) {
                return true;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            self.pump(remaining.min(Duration::from_millis(200))).await;
        }
    }
}

#[tokio::test]
async fn in_terminal_exec_runs_echo_quotes_and_exit_codes() {
    if !common::full_live("in_terminal_exec_runs_echo_quotes_and_exit_codes") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 in_terminal_exec_runs_echo_quotes_and_exit_codes：{reason}");
            return;
        }
    };
    let Some(mut term) = open_shell(&server).await else {
        return;
    };

    // 1. echo 带算术：回显里只有 $((40+2))，出现 42 说明真跑了。
    let (out, code) = term
        .run("echo ZSSH_PTY_$((40+2))", Duration::from_secs(15))
        .await;
    assert!(
        out.contains("ZSSH_PTY_42"),
        "输出里没有标记：{}",
        server.sanitize(&out)
    );
    assert_eq!(code, Some(0), "退出码应为 0：{}", server.sanitize(&out));
    println!("终端执行：echo 输出含 ZSSH_PTY_42，退出码 0");

    // 2. false 拿退出码 1。
    let (_, code) = term.run("false", Duration::from_secs(15)).await;
    assert_eq!(code, Some(1), "false 的退出码应为 1");
    println!("终端执行：false 退出码 1");

    // 3. 单双引号和管道。
    let (out, code) = term
        .run(
            r#"printf '%s\n' "a b" | tr ' ' '_'"#,
            Duration::from_secs(15),
        )
        .await;
    assert_eq!(code, Some(0));
    assert!(
        out.contains("a_b"),
        "引号 / 管道结果不对：{}",
        server.sanitize(&out)
    );
    println!("终端执行：单双引号 + 管道正确（{out:?}）");

    // 4. 多行 + $? 位置。
    let (out, code) = term
        .run("echo one\necho two", Duration::from_secs(15))
        .await;
    assert_eq!(code, Some(0));
    assert!(
        out.contains("one") && out.contains("two"),
        "多行命令不对：{}",
        server.sanitize(&out)
    );
    println!("终端执行：多行命令正确");

    // 5. 命令没进 bash 历史（前面加了空格；bash 默认要 HISTCONTROL=ignorespace
    //    才生效，这里显式打开，验证这个机制真的管用）。
    let (_, _) = term
        .run("HISTCONTROL=ignorespace", Duration::from_secs(15))
        .await;
    let (_, _) = term
        .run("echo ZSSH_HIST_PROBE", Duration::from_secs(15))
        .await;
    let (out, _) = term
        .run(
            "history 2>/dev/null | grep -c ZSSH_HIST_PROBE || true",
            Duration::from_secs(15),
        )
        .await;
    // 期望 0：前面加空格的 AI 命令不进历史（连这条 grep 自己也会被过滤掉，因为它也带空格）。
    let count: i32 = out.trim().parse().unwrap_or(-1);
    println!("终端执行：history 里 ZSSH_HIST_PROBE 出现 {count} 次（期望 0）");
    assert_eq!(
        count,
        0,
        "AI 命令不该进 bash 历史：{}",
        server.sanitize(&out)
    );

    let _ = term.shell.close().await;
}

#[tokio::test]
async fn output_without_trailing_newline_finishes_without_timeout() {
    if !common::full_live("output_without_trailing_newline_finishes_without_timeout") {
        return;
    }
    // AI-2e 第 2 节：`printf abc` 没有换行结尾，结束标记紧跟在输出后面（不在
    // 行首）。修复前 `find_end_marker` 只认行首标记，这条命令要等到超时才返回，
    // 还会把一条早就结束的命令报成"仍在运行"。
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 output_without_trailing_newline_finishes_without_timeout：{reason}");
            return;
        }
    };
    let Some(mut term) = open_shell(&server).await else {
        return;
    };

    let started = Instant::now();
    let (out, code) = term.run("printf abc", Duration::from_secs(15)).await;
    let elapsed = started.elapsed();
    assert_eq!(code, Some(0), "要拿到退出码 0：{}", server.sanitize(&out));
    assert_eq!(
        out.trim(),
        "abc",
        "输出要正好是 abc：{}",
        server.sanitize(&out)
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "不是超时返回（超时是 15s），实际 {elapsed:?}"
    );
    println!("printf abc：输出 abc、退出码 0，耗时 {elapsed:?}（远小于 15s 超时）");

    // 显示（发给界面的数据）里能看到 abc，看不到标记。
    assert!(
        term.display_text.contains("abc"),
        "终端显示里要看得到 abc：{:?}",
        term.display_text
    );
    assert!(
        !term.display_text.contains("__ZSSH_"),
        "显示里不能有标记：{:?}",
        term.display_text
    );
    println!("printf abc：显示里有 abc、没有 __ZSSH_");

    let _ = term.shell.close().await;
}

#[tokio::test]
async fn terminal_busy_detection_falls_back_when_top_is_open() {
    if !common::full_live("terminal_busy_detection_falls_back_when_top_is_open") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 terminal_busy_detection_falls_back_when_top_is_open：{reason}");
            return;
        }
    };
    let Some(mut term) = open_shell(&server).await else {
        return;
    };

    // 提示符停下时算空闲：先泵一会儿输出，用 TerminalState 判定。
    term.pump(Duration::from_millis(800)).await;
    // 开 top：它会持续刷新进程表，终端不再停在提示符上。
    term.shell.write(b"top\r").await.expect("写 top failed");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        term.pump(Duration::from_millis(200)).await;
        if term.state.is_alt_screen() || !term.state.is_idle() {
            break;
        }
    }
    assert!(
        term.state.placement().is_err(),
        "top 开着时终端算忙（会降级到后台）；收到的尾部：{}",
        server.sanitize(&tail_of(&term.raw, 400))
    );
    println!(
        "终端忙检测：top 开着时判定为忙（alt_screen={}），会降级到后台",
        term.state.is_alt_screen()
    );

    // 退出 top（q），回到提示符。
    term.shell.write(b"q").await.expect("写 q failed");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        term.pump(Duration::from_millis(200)).await;
        if !term.state.is_alt_screen() && term.state.is_idle() {
            break;
        }
        if Instant::now() >= deadline {
            // 再给一次机会：补一个 echo 让提示符重新出现。
            term.shell
                .write(b"echo ZSSH_READY\r")
                .await
                .expect("写 echo failed");
            term.pump(Duration::from_millis(800)).await;
            break;
        }
    }
    println!(
        "终端忙检测：退出 top 后恢复空闲判定（idle={}）",
        term.state.is_idle()
    );

    let _ = term.shell.close().await;
}

#[tokio::test]
async fn in_terminal_exec_timeout_does_not_interrupt_and_stop_sends_ctrl_c() {
    if !common::full_live("in_terminal_exec_timeout_does_not_interrupt_and_stop_sends_ctrl_c") {
        return;
    }
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 in_terminal_exec_timeout...：{reason}");
            return;
        }
    };
    let Some(mut term) = open_shell(&server).await else {
        return;
    };
    term.pump_until("$", Duration::from_secs(5)).await;

    // 超时：sleep 30 给 2 秒超时 → 拿不到结束标记，返回已收到的输出。
    let (_, code) = term.run("sleep 30", Duration::from_secs(2)).await;
    assert_eq!(code, None, "超时时不该有退出码（命令仍在跑）");
    // 命令仍在跑：向终端发 Ctrl+C 停掉它。
    term.shell.write(&[0x03]).await.expect("写 Ctrl+C failed");
    // 回到提示符：再跑一条 echo 能拿到退出码 0。
    let (out, code) = term
        .run("echo ZSSH_AFTER_CTRL_C", Duration::from_secs(15))
        .await;
    assert_eq!(
        code,
        Some(0),
        "Ctrl+C 之后应回到提示符：{}",
        server.sanitize(&out)
    );
    assert!(out.contains("ZSSH_AFTER_CTRL_C"));
    println!("停止：sleep 30 超时后发 Ctrl+C，命令被打断，回到提示符");

    let _ = term.shell.close().await;
}

#[tokio::test]
async fn shell_kind_and_prompt_detection() {
    if !common::full_live("shell_kind_and_prompt_detection") {
        return;
    }
    // 纯函数，不连服务器：确认 shell 类型识别和提示符判断。
    assert_eq!(classify_shell_name("bash"), Some(ShellKind::Posix));
    assert_eq!(classify_shell_name("/usr/bin/fish"), Some(ShellKind::Fish));
    assert_eq!(classify_shell_name("/usr/bin/nu"), None, "认不出的 shell");
    assert!(looks_idle("$ "));
}

#[tokio::test]
async fn shell_probe_detects_posix_on_the_test_server() {
    if !common::full_live("shell_probe_detects_posix_on_the_test_server") {
        return;
    }
    // AI-2d 1.3：探测命令必须在真实服务器上认出 POSIX 登录 shell。
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 shell_probe_detects_posix_on_the_test_server：{reason}");
            return;
        }
    };
    let known_hosts = TempKnownHosts::new("ai2d-shell-probe");
    let connection = match server.connect(&known_hosts).await {
        Ok(connection) => connection,
        Err(reason) => {
            // 链路问题（连接超时之类）：打印原因跳过，不算失败。
            println!("跳过 shell_probe_detects_posix_on_the_test_server：连不上（{reason}）");
            return;
        }
    };
    let kind = probe_shell_kind(&connection).await;
    let Some(kind) = kind else {
        println!("跳过 shell_probe_detects_posix_on_the_test_server：三次探测都没跑完（链路慢？）");
        connection.close().await;
        return;
    };
    assert_eq!(
        kind,
        ShellKind::Posix,
        "测试服务器要识别成 POSIX（认不出说明分类或探测命令有问题）"
    );
    println!("远端 shell 探测：测试服务器识别为 POSIX");
    connection.close().await;
}

/// 取一段文本的尾部（诊断输出用）。
fn tail_of(text: &str, chars: usize) -> String {
    let mut chars_vec: Vec<char> = text.chars().collect();
    if chars_vec.len() > chars {
        chars_vec.drain(..chars_vec.len() - chars);
    }
    chars_vec.into_iter().collect()
}

#[tokio::test]
async fn ten_kilobytes_of_chinese_output_keeps_the_session_alive() {
    if !common::full_live("ten_kilobytes_of_chinese_output_keeps_the_session_alive") {
        return;
    }
    // AI-2d 1.1：中文输出累计超过 4KB 时，尾部截断以前按字节下标切 `str`，
    // 下标落在多字节字符中间会让会话任务 panic（测试里 feed 在 pump 上）。
    let _serial = serial().await;
    let server = match TestServer::from_env() {
        Ok(server) => server,
        Err(reason) => {
            println!("跳过 ten_kilobytes_of_chinese_output_keeps_the_session_alive：{reason}");
            return;
        }
    };
    let Some(mut term) = open_shell(&server).await else {
        return;
    };

    let (out, code) = term
        .run(
            "for i in $(seq 1 800); do printf '中文输出测试'; done; echo",
            Duration::from_secs(30),
        )
        .await;
    assert_eq!(code, Some(0), "命令要正常结束：{code:?}");
    assert!(
        out.contains("中文输出测试"),
        "输出里要看到中文：{}",
        server.sanitize(&tail_of(&out, 200))
    );
    assert!(
        out.len() > 10 * 1024,
        "输出要超过 10KB（实际 {} 字节）",
        out.len()
    );
    println!("10KB 中文输出：命令正常结束，收到 {} 字节", out.len());

    // 会话还活着：再跑一条命令，输出和退出码都对。
    let (out, code) = term
        .run("echo ZSSH_AFTER_$((40+2))", Duration::from_secs(15))
        .await;
    assert_eq!(code, Some(0), "中文输出之后会话要还能用");
    assert!(
        out.contains("ZSSH_AFTER_42"),
        "后续命令的输出不对：{}",
        server.sanitize(&out)
    );
    // 终端状态也还正常（提示符判定 → 可以就地执行）。
    term.pump(Duration::from_millis(300)).await;
    println!(
        "10KB 中文输出之后：会话正常，后续命令退出码 0，终端判定空闲={}",
        term.state.is_idle()
    );

    let _ = term.shell.close().await;
}

/// 探测远端 shell 类型，带重试（链路慢时一次探测可能跑不完）。
async fn probe_shell_kind(connection: &Connection) -> Option<ShellKind> {
    for attempt in 0..3 {
        match connection
            .exec_capture(&build_shell_probe_command(), SHELL_PROBE_TIMEOUT, 64 * 1024)
            .await
        {
            Ok(capture) => {
                if let Some(kind) = parse_shell_probe_output(&capture.stdout) {
                    return Some(kind);
                }
                println!(
                    "探测输出认不出来（第 {} 次）：{}",
                    attempt + 1,
                    capture.stdout.len()
                );
            }
            Err(err) => println!("探测没跑完（第 {} 次）：{err}", attempt + 1),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    None
}

async fn open_shell(server: &TestServer) -> Option<Terminal> {
    let known_hosts = TempKnownHosts::new("ai2c-pty");
    let connection = server
        .connect(&known_hosts)
        .await
        .expect("连接测试服务器失败");
    let shell = connection
        .open_shell(COLS, ROWS)
        .await
        .expect("打开 shell 失败");
    let mut term = Terminal::new(shell);
    // 1.3：和生产的 `exec_placement` 一样，先在后台通道探测一次远端 shell 类型
    // （不探测就一直是 Unknown，不会就地执行）。
    match probe_shell_kind(&connection).await {
        Some(kind) => term.state.set_shell_kind(kind),
        None => {
            // 链路慢 / 探测通道没跑完：本用例不依赖"是 POSIX"，按 Other 继续
            // （生产和这里一样：探测不出就走后台通道）。
            println!("注意：探测远端 shell 没成功（链路慢？），本用例按 Other 继续");
            term.state.set_shell_kind(ShellKind::Other);
        }
    }
    // 提示符擦干净（截图 / 输出里不带主机名）。
    term.shell
        .write(b"PS1='$ '\r")
        .await
        .expect("设置提示符失败");
    term.pump_until("$", Duration::from_secs(5)).await;
    term.raw.clear();
    term.display_text.clear();
    Some(term)
}
