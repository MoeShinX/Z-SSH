//! 管理测试服务器上 SFTP 线测试遗留的临时目录（M3-2 第 0 节）。
//!
//! 线测试的临时目录都在 `/tmp/zssh-sftp-live-<随机串>`。守卫对象只能保证"本次运行"
//! 一定删掉；历史上（M3-1 验收时）已经留下过一批，这个工具用来查看和清理：
//!
//! ```text
//! cargo run -p zssh-core --example sftp_tmp_admin -- list    # 列出（只看不动）
//! cargo run -p zssh-core --example sftp_tmp_admin -- sweep   # 删掉 1 小时前的
//! cargo run -p zssh-core --example sftp_tmp_admin -- sweep 0 # 全删（不管多新）
//! cargo run -p zssh-core --example sftp_tmp_admin -- sweep 0 zssh-sftp-m32-
//!                                                    # 换前缀（端到端脚本的临时目录）
//! ```
//!
//! 凭据只从 `ZSSH_TEST_*` 环境变量读（和线测试同一套规矩）；缺了就退出。
//! **只碰 `zssh-sftp-live-` 前缀的目录，别的什么都不动。**

use std::sync::Arc;
use std::time::SystemTime;

use zssh_core::sftp::fs::FileKind;
use zssh_core::sftp::session::SftpChannel;
use zssh_core::ssh::{AuthConfig, ConnectParams, Connection, KnownHosts, NonInteractive};

/// 和线测试同一个前缀（`tests/common` 里的 `REMOTE_TEMP_PREFIX`）。
const PREFIX: &str = "zssh-sftp-live-";

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    // 可选的第三个参数：换前缀（端到端脚本的临时目录是 zssh-sftp-m32-*）。
    // 只允许 zssh-sftp- 开头，免得手滑把 /tmp 里别的东西删了。
    let prefix = std::env::args().nth(3).unwrap_or_else(|| PREFIX.to_owned());
    if !prefix.starts_with("zssh-sftp-") {
        eprintln!("前缀必须以 zssh-sftp- 开头（只清自己的测试目录）");
        std::process::exit(2);
    }
    let min_age_secs: u64 = match mode.as_str() {
        "list" => u64::MAX, // list 模式永远不删
        "sweep" => std::env::args()
            .nth(2)
            .and_then(|value| value.parse().ok())
            .unwrap_or(3600),
        _ => {
            eprintln!("用法：sftp_tmp_admin <list|sweep [最小树龄秒数，默认 3600]>");
            std::process::exit(2);
        }
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("建 runtime");
    runtime.block_on(async move {
        let (host, port, username, password) = match server_from_env() {
            Ok(value) => value,
            Err(reason) => {
                eprintln!("{reason}");
                std::process::exit(2);
            }
        };
        // 目录守卫：本次只信任、基本不会落盘，但退出时整个目录都会删掉。
        let known_hosts_dir = tempfile::Builder::new()
            .prefix("zssh-sftp-admin-")
            .tempdir()
            .expect("建临时目录");
        let known_hosts_path = known_hosts_dir.path().join("known_hosts");
        let params = ConnectParams {
            host,
            port,
            username,
            auth: AuthConfig::password(Some(password)),
            known_hosts: KnownHosts::new(&known_hosts_path),
            legacy_algorithms: false,
        };
        // 连接错误里带服务器地址：换掉再输出，和联网测试一个规矩。
        let host_for_log = params.host.clone();
        let connection = match Connection::connect(params, Arc::new(NonInteractive)).await {
            Ok(connection) => Arc::new(connection),
            Err(err) => {
                eprintln!(
                    "连接测试服务器失败：{}",
                    err.to_string().replace(&host_for_log, "测试服务器")
                );
                // process::exit 不跑 Drop，先手动放掉守卫。
                drop(known_hosts_dir);
                std::process::exit(1);
            }
        };
        let channel = SftpChannel::open(&connection).await.expect("开 SFTP 通道");

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("系统时钟在 1970 之前")
            .as_secs();
        let entries = channel.read_dir("/tmp").await.expect("列 /tmp");
        let mut matches = Vec::new();
        for entry in entries {
            if !entry.name.starts_with(prefix.as_str()) || !matches!(entry.kind, FileKind::Dir) {
                continue;
            }
            let age = entry.mtime.map(|mtime| now.saturating_sub(mtime as u64));
            matches.push((entry.name.clone(), age));
        }

        println!("/tmp 里匹配 {prefix}* 的目录 {} 个：", matches.len());
        let mut removed = 0usize;
        for (name, age) in &matches {
            let target = format!("/tmp/{name}");
            let keep = age.is_some_and(|age| age < min_age_secs);
            let age_text = match age {
                Some(age) => format!("{age} 秒前修改"),
                None => "修改时间未知".to_owned(),
            };
            if mode == "list" || keep {
                println!("  {target}（{age_text}）保留");
                continue;
            }
            // 先走自己的递归删除；删不动（非 UTF-8 名字）就 rm -rf 兜底。
            let deleter = zssh_core::sftp::RecursiveDelete::new();
            let _ = deleter.run(&channel, &target, |_| {}).await;
            if channel.lstat(&target).await.is_ok() {
                let _ = connection
                    .exec(&format!("rm -rf -- '{target}'"))
                    .await
                    .is_ok();
            }
            if channel.lstat(&target).await.is_ok() {
                println!("  {target}（{age_text}）删除失败");
            } else {
                removed += 1;
                println!("  {target}（{age_text}）已删除");
            }
        }
        println!(
            "共 {} 个匹配，删除 {} 个，保留 {} 个",
            matches.len(),
            removed,
            matches.len() - removed
        );

        channel.close();
        let _ = connection.disconnect().await;
    });
}

/// 从环境变量拿测试服务器信息；错误信息里只有变量名，没有值。
fn server_from_env() -> Result<(String, u16, String, String), String> {
    let host = require("ZSSH_TEST_HOST")?;
    let port = match std::env::var("ZSSH_TEST_PORT") {
        Ok(raw) if !raw.trim().is_empty() => raw
            .trim()
            .parse::<u16>()
            .map_err(|_| "ZSSH_TEST_PORT 不是合法端口".to_owned())?,
        _ => 22,
    };
    let username = require("ZSSH_TEST_USER")?;
    let password = require("ZSSH_TEST_PASSWORD")?;
    Ok((host, port, username, password))
}

fn require(name: &str) -> Result<String, String> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() => Ok(value),
        _ => Err(format!("缺少环境变量 {name}")),
    }
}
