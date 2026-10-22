//! AI 面板（AI-2b）的纯函数测试：工具输入解析、友好名称、主机名和退出码。

import { describe, expect, it } from "vitest";

import {
  agentAvailabilityOf,
  commandOf,
  exitCodeOf,
  friendlyAgentError,
  friendlyToolName,
  hostDotClass,
  hostDotTitle,
  hostOf,
  hostStateOf,
  hostTargetId,
  isAborted,
  isDangerous,
  sessionIdOf,
  skippedOlderNote,
} from "./aiHelpers";

describe("AI 面板：工具输入解析", () => {
  it("run_command 的 input 里有 command", () => {
    expect(commandOf({ session_id: "s1", command: "uname -a" })).toBe("uname -a");
  });

  it("没有 command 字段时返回 null", () => {
    expect(commandOf({ path: "/etc/hosts" })).toBeNull();
    expect(commandOf(null)).toBeNull();
    expect(commandOf("raw")).toBeNull();
  });

  it("危险等级来自 input.danger", () => {
    expect(isDangerous({ command: "rm -rf /", danger: "dangerous" })).toBe(true);
    expect(isDangerous({ command: "ls", danger: "safe" })).toBe(false);
    expect(isDangerous(null)).toBe(false);
  });

  it("会话 id 从 input.session_id 取", () => {
    expect(sessionIdOf({ session_id: "s1" })).toBe("s1");
    expect(sessionIdOf({})).toBeNull();
  });
});

describe("AI 面板：工具卡片友好名称", () => {
  it("把工具 id 映射成中文名", () => {
    expect(friendlyToolName("zssh_run_command")).toBe("执行命令");
    expect(friendlyToolName("zssh_list_targets")).toBe("列出服务器");
    expect(friendlyToolName("zssh_read_terminal")).toBe("读取终端");
    // 内置工具没映射就照原样。
    expect(friendlyToolName("bash")).toBe("bash");
    expect(friendlyToolName("")).toBe("工具调用");
  });

  it("未连接主机的目标 ID 是 host-<id>", () => {
    expect(hostTargetId(7)).toBe("host-7");
  });
});

describe("AI 面板：主机名和退出码", () => {
  it("按会话 id 查主机显示名", () => {
    const hosts = { s1: "web-1", s2: "db-1" };
    expect(hostOf({ session_id: "s1" }, hosts)).toBe("web-1");
    expect(hostOf({ session_id: "s9" }, hosts)).toBeNull();
    expect(hostOf({}, hosts)).toBeNull();
  });

  it("从工具输出文本里抠退出码", () => {
    expect(exitCodeOf("danger: safe\nexit_code: 0\nduration_ms: 12")).toBe(0);
    expect(exitCodeOf("exit_code: 3")).toBe(3);
    expect(exitCodeOf("no code here")).toBeNull();
  });
});

describe("AI 面板：@ 标签（AI-2d 第 3 节）", () => {
  it("状态点反映真实状态，不是固定琥珀色", () => {
    expect(hostDotClass("connected")).toBe("bg-emerald-500");
    expect(hostDotClass("connecting")).toContain("bg-amber-400");
    expect(hostDotClass("offline")).toBe("bg-muted-foreground/50");
    expect(hostDotTitle("connected")).toContain("已连接");
    expect(hostDotTitle("connecting")).toContain("连接中");
    expect(hostDotTitle("offline")).toContain("还没连上");
  });

  it("没有状态记录的主机算还没连上", () => {
    expect(hostStateOf({}, 5)).toBe("offline");
    expect(hostStateOf({ 5: "connected" }, 5)).toBe("connected");
  });
});

describe("AI 面板：用户主动停止（AI-2e 第 3 节）", () => {
  it("中止会话 / 中止工具的错误文本能识别出来，卡片显示已停止而不是失败", () => {
    expect(isAborted("MessageAbortedError: Aborted")).toBe(true);
    expect(isAborted("Tool execution aborted")).toBe(true);
    expect(isAborted("boom")).toBe(false);
    expect(isAborted("")).toBe(false);
  });
});

describe("被跳过的旧版本提示（M3-1 第 0 节）", () => {
  const installWith = (older: { version: string; source: "path" | "known_location" } | null) => ({
    path: "C:\tools\opencode.exe",
    version: "1.19.1",
    source: "known_location" as const,
    needs_shell: false,
    auto: true,
    too_old: false,
    skipped_older: older,
  });

  it("PATH 上还有旧版本时给一句提示（带版本号）", () => {
    const note = skippedOlderNote(installWith({ version: "1.17.11", source: "path" }));
    expect(note).toContain("PATH");
    expect(note).toContain("1.17.11");
  });

  it("没有被跳过的版本就不提示", () => {
    expect(skippedOlderNote(installWith(null))).toBeNull();
  });
});

describe("AI 面板：多智能体可用状态（AI-3）", () => {
  const install = (over: Partial<{ too_old: boolean }> = {}) => ({
    path: "C:\tools\codex.exe",
    version: "0.162.1",
    source: "path" as const,
    needs_shell: false,
    auto: true,
    too_old: false,
    skipped_older: null,
    ...over,
  });

  it("还在检测 / 找到 / 没找到", () => {
    expect(agentAvailabilityOf(undefined)).toBe("ok");
    expect(agentAvailabilityOf(install())).toBe("ok");
    expect(agentAvailabilityOf(null)).toBe("missing");
  });

  it("版本过低和没登录各有各的原因", () => {
    expect(agentAvailabilityOf(install({ too_old: true }))).toBe("old");
    expect(agentAvailabilityOf(install(), { logged_in: false, hint: "先登录" })).toBe(
      "login",
    );
    expect(agentAvailabilityOf(install(), { logged_in: true, hint: null })).toBe("ok");
    // 登录状态是 null（opencode 那种没有登录概念的）：只看安装状态。
    expect(agentAvailabilityOf(install(), { logged_in: null, hint: null })).toBe("ok");
  });
});

import {
  backupPathOf,
  bareToolName,
  DIFF_FOLD_LIMIT,
  diffSummary,
  fileDangerReason,
  foldDiff,
  isFileTool,
  ownerChangeOf,
  readableOutput,
  type DiffLineView,
} from "./aiHelpers";

describe("文件工具（M3-4）：名称与路径", () => {
  it("各智能体给工具加的前缀都能还原成工具名", () => {
    expect(bareToolName("zssh_write_file")).toBe("write_file");
    expect(bareToolName("mcp__zssh__read_file")).toBe("read_file");
    expect(bareToolName("write_file")).toBe("write_file");
    expect(isFileTool("zssh_list_dir")).toBe(true);
    expect(isFileTool("zssh_stat")).toBe(true);
    expect(isFileTool("zssh_run_command")).toBe(false);
    expect(isFileTool("write")).toBe(false);
  });

  it("友好名称", () => {
    expect(friendlyToolName("zssh_list_dir")).toBe("列出目录");
    expect(friendlyToolName("zssh_read_file")).toBe("读取文件");
    expect(friendlyToolName("zssh_write_file")).toBe("写入文件");
    expect(friendlyToolName("zssh_stat")).toBe("查看文件信息");
  });
});

describe("文件工具：差异折叠", () => {
  const lines = (n: number): DiffLineView[] =>
    Array.from({ length: n }, (_, i) => ({ kind: "add" as const, text: `line ${i}` }));

  it("不长就全部显示", () => {
    expect(foldDiff(lines(DIFF_FOLD_LIMIT), false)).toEqual({
      visible: lines(DIFF_FOLD_LIMIT),
      hidden: 0,
    });
  });

  it("很长就折叠，说明折了多少行；展开后全部显示", () => {
    const folded = foldDiff(lines(200), false);
    expect(folded.visible).toHaveLength(DIFF_FOLD_LIMIT);
    expect(folded.hidden).toBe(200 - DIFF_FOLD_LIMIT);
    expect(foldDiff(lines(200), true).hidden).toBe(0);
    expect(foldDiff(lines(200), true).visible).toHaveLength(200);
  });

  it("摘要", () => {
    expect(diffSummary({ added: 3, removed: 0, is_new: true })).toBe("新建文件，共 3 行");
    expect(diffSummary({ added: 2, removed: 1, is_new: false })).toBe("+2 −1");
    expect(diffSummary({ added: 0, removed: 0, is_new: false })).toBe("内容没有变化");
  });
});

describe("文件工具：从结果文本里取标红原因和备份", () => {
  const output =
    "path: /etc/hosts\nwritten_bytes: 10\nreplaced: the existing file was overwritten\n" +
    "backup: the previous version was saved on the server at /etc/hosts.zssh-bak-20260101-000000\n" +
    "danger: flagged as a system path (写入系统路径：系统配置目录 /etc)\n";

  it("取得到", () => {
    expect(fileDangerReason(output)).toBe("写入系统路径：系统配置目录 /etc");
    expect(backupPathOf(output)).toBe("/etc/hosts.zssh-bak-20260101-000000");
  });

  it("智能体把结果包成 JSON 字符串时，路径和原因不带上引号和字面的 \n", () => {
    const wrapped =
      '[{"text":"path: /etc/hosts\\nbackup: the previous version was saved on the server at /etc/hosts.zssh-bak-1\\n' +
      'danger: flagged as a system path (写入系统路径：系统配置目录 /etc)\\n","type":"text"}]';
    expect(backupPathOf(wrapped)).toBe("/etc/hosts.zssh-bak-1");
    expect(fileDangerReason(wrapped)).toBe("写入系统路径：系统配置目录 /etc");
  });

  it("没有标红 / 新建文件时取不到", () => {
    expect(fileDangerReason("path: /tmp/x\nbackup: none (new file)\n")).toBeNull();
    expect(backupPathOf("path: /tmp/x\nbackup: none (new file)\n")).toBeNull();
  });
});

describe("工具卡片的输出：拆开智能体包的 JSON", () => {
  it("把 MCP 内容数组拆成带真实换行的文本", () => {
    const wrapped =
      '[{"text":"path: /home/u/notes.txt\\nsize: 49\\ncontent:\\nhello\\n","type":"text"}]';
    expect(readableOutput(wrapped)).toBe(
      "path: /home/u/notes.txt\nsize: 49\ncontent:\nhello\n",
    );
  });

  it("多层包（JSON 字符串里再套数组）也拆得开", () => {
    const inner = '[{"text":"line one\\nline two","type":"text"}]';
    expect(readableOutput(JSON.stringify(inner))).toBe("line one\nline two");
  });

  it("多个文本块按顺序拼起来", () => {
    const wrapped =
      '[{"text":"a\\n","type":"text"},{"text":"b\\n","type":"text"}]';
    expect(readableOutput(wrapped)).toBe("a\nb\n");
  });

  it("不是 JSON / 认不出的 JSON 原样显示，不吞输出", () => {
    expect(readableOutput("plain text\nsecond line")).toBe("plain text\nsecond line");
    expect(readableOutput('{"other":1}')).toBe('{"other":1}');
    expect(readableOutput("[1,2,3]")).toBe("[1,2,3]");
  });

  it("属主变化的提示从拆包后的文本里取", () => {
    const wrapped =
      '[{"text":"path: /etc/hosts\\nowner: could not restore the file\'s owner: ' +
      'it changed from uid 0 / gid 0 to uid 1000 / gid 1000 (permission denied)\\n","type":"text"}]';
    expect(ownerChangeOf(wrapped)).toEqual({
      from: "uid 0 / gid 0",
      to: "uid 1000 / gid 1000",
    });
  });
});

describe("AI 面板：opencode 免费服务报错改写", () => {
  it("版本太旧", () => {
    const text = friendlyAgentError("OpenCode 1.18.0 or newer is required to use the free tier");
    expect(text).toContain("opencode 版本太旧");
    expect(text).toContain("1.18");
    expect(text).toContain("npm i -g opencode-ai@latest");
  });

  it("免费服务拒绝：不再说只能在 opencode 自己的界面里用", () => {
    const text = friendlyAgentError("OpenCode's free tier can only be used from within OpenCode");
    expect(text).toContain("OpenCode 的免费服务拒绝了这次请求");
    expect(text).toContain("换一个非免费模型");
    expect(text).not.toContain("自己的界面");
  });

  it("其他错误原样显示", () => {
    expect(friendlyAgentError("boom")).toBe("boom");
    expect(friendlyAgentError("")).toBe("");
  });
});
