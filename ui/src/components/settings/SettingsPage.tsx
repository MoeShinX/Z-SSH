// Adapted from Netcatty components/SettingsPage.tsx（视觉：左侧分组导航 + 右侧内容）
//! 设置页：外观 / 终端 / 连接 / 快捷键 / 关于。改一个值立刻生效（存进 settings 表），
//! 没有保存按钮。

import { getVersion } from "@tauri-apps/api/app";
import {
  Bot,
  CloudUpload,
  FileText,
  FolderOpen,
  Keyboard,
  Info,
  Link2,
  MonitorCog,
  Network,
  RefreshCw,
  SquareTerminal,
  X,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";

import { aboutStatusText } from "../../state/updateModel";
import { useUngroupedFlat } from "../../state/ungroupedFlat";
import { useUpdater } from "../../stores/updater";
import UpdateProgress from "../sync/UpdateProgress";
import NOTICE_TEXT from "../../assets/NOTICE.txt?raw";
import {
  appNetworkSystemProxy,
  dataDirInfo,
  openDataDir,
  settingsGet,
  settingsSet,
  type SystemProxy,
} from "../../api/settings";
import {
  APP_NETWORK_PROXY_KEY,
  appProxyChoice,
  appProxyValue,
  systemProxyText,
} from "../../state/appNetwork";
import {
  aiAgentDiscover,
  aiAgentDownload,
  aiAgentDownloadInfo,
  aiAgentGuard,
  aiAgentLogin,
  aiAgentProbePath,
  onDownloadProgress,
  type AgentId,
  type AgentInstall,
  type AgentLoginStatus,
  type ReleaseInfo,
} from "../../api/agent";
import { defaultProxyGet, defaultProxySet } from "../../api/vault";
import { cn } from "../../lib/utils";
import { useSettings } from "../../stores/settings";
import { useSync } from "../../stores/sync";
import { useVault } from "../../stores/vault";
import { Button } from "../ui/button";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogTrigger } from "../ui/dialog";
import { Input } from "../ui/input";
import { ScrollArea } from "../ui/scroll-area";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";
import { Switch } from "../ui/switch";
import { toast } from "../ui/toast";
import { skippedOlderNote } from "../ai/aiHelpers";
import { ClearAiHistoryRow } from "../ai/ClearHistoryRow";
import { downloadAndRecheck, errorText } from "./opencodeDownload";
import { SectionCard, SettingRow } from "./settingsUi";
import HighlightSection from "./HighlightSection";
import SessionLogSection from "./SessionLogSection";
import SyncSection from "./sync/SyncSection";
import {
  SkinGrid,
  TerminalMismatchHint,
  TerminalThemeSelect,
  ThemeModeButtons,
} from "./AppearancePickers";
import { EXEC_MODE_OPTIONS, toExecMode } from "../../state/execMode";
import { parseCloseAction, parseFileWindowCloseAction } from "../../state/closeBehavior";

type SectionId =
  | "appearance"
  | "terminal"
  | "connection"
  | "sftp"
  | "ai"
  | "sync"
  | "shortcuts"
  | "about";

const SECTIONS: { id: SectionId; label: string; icon: React.ReactNode }[] = [
  { id: "appearance", label: "外观", icon: <MonitorCog size={15} /> },
  { id: "terminal", label: "终端", icon: <SquareTerminal size={15} /> },
  { id: "connection", label: "连接", icon: <Link2 size={15} /> },
  { id: "sftp", label: "SFTP", icon: <FolderOpen size={15} /> },
  { id: "ai", label: "AI", icon: <Bot size={15} /> },
  { id: "sync", label: "同步", icon: <CloudUpload size={15} /> },
  { id: "shortcuts", label: "快捷键", icon: <Keyboard size={15} /> },
  { id: "about", label: "关于", icon: <Info size={15} /> },
];

interface SettingsPageProps {
  /** 关掉设置页，回到进来之前那一页（右上角 ×；齿轮和 Esc 在 App 里接）。 */
  onClose?: () => void;
  /** 打开代理面板（「连接」里默认代理旁的「新建 / 管理代理」）。 */
  onManageProxies?: () => void;
  /** 代理面板开着没有：关掉时「连接」页重读默认代理（面板里也能改默认代理）。 */
  proxyPanelOpen?: boolean;
}

export default function SettingsPage({
  onClose,
  onManageProxies,
  proxyPanelOpen = false,
}: SettingsPageProps = {}) {
  // 从状态栏点进来时直接打开“同步”分组。
  const [section, setSection] = useState<SectionId>(() =>
    useSync.getState().wantSettingsSection ? "sync" : "appearance",
  );
  useEffect(() => {
    useSync.getState().requestSettingsSection(false);
  }, []);

  return (
    <div data-section="settings" className="flex h-full min-h-0">
      <aside className="w-44 shrink-0 border-r border-border/60 p-3">
        <h2 className="px-2 pb-2 text-xs font-semibold uppercase tracking-wide text-muted-foreground">
          设置
        </h2>
        <div className="space-y-0.5">
          {SECTIONS.map((item) => (
            <button
              key={item.id}
              onClick={() => setSection(item.id)}
              className={cn(
                "flex w-full cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-left text-[13px] transition-colors",
                section === item.id
                  ? "bg-accent font-medium text-accent-foreground"
                  : "hover:bg-accent/60 hover:text-accent-foreground",
              )}
            >
              {item.icon}
              {item.label}
            </button>
          ))}
        </div>
      </aside>

      <div className="relative flex min-h-0 min-w-0 flex-1">
      {onClose && (
        // 以前设置页没有出口，只能点左上角「主机」，从终端进来的人也被带回主机页（R-11）
        <button
          type="button"
          onClick={onClose}
          title="关闭设置（Esc）"
          aria-label="关闭设置"
          className="absolute right-3 top-3 z-10 cursor-pointer rounded-md p-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        >
          <X size={18} />
        </button>
      )}
      <ScrollArea className="min-h-0 flex-1">
        <div className="mx-auto max-w-3xl space-y-4 p-6">
          {section === "appearance" && <AppearanceSection />}
          {section === "terminal" && <TerminalSection />}
          {section === "connection" && (
            <ConnectionSection onManageProxies={onManageProxies} proxyPanelOpen={proxyPanelOpen} />
          )}
          {section === "sftp" && <SftpSection />}
          {section === "ai" && <AISection />}
          {section === "sync" && <SyncSection />}
          {section === "shortcuts" && <ShortcutsSection />}
          {section === "about" && <AboutSection />}
        </div>
      </ScrollArea>
      </div>
    </div>
  );
}

/** 滑块 + 数值显示。 */
function Slider({
  value,
  min,
  max,
  step = 1,
  suffix,
  onChange,
}: {
  value: number;
  min: number;
  max: number;
  step?: number;
  suffix?: string;
  onChange: (value: number) => void;
}) {
  return (
    <div className="flex w-64 items-center gap-3">
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(event) => onChange(Number(event.target.value))}
        className="h-1.5 w-full cursor-pointer appearance-none rounded-full bg-secondary accent-[hsl(var(--primary))]"
      />
      <span className="w-16 shrink-0 text-right font-mono text-xs text-muted-foreground">
        {value}
        {suffix ?? ""}
      </span>
    </div>
  );
}

/**
 * 关闭主窗口时缩到托盘还是退出程序（UI-4），关闭 Z-SFTP 文件窗口时断开还是合并回主窗口（UI-5）。
 * 都是本机习惯，不参与配置同步。
 */
function WindowCard() {
  const closeAction = useSettings((state) => state.close_action);
  const fileWindowClose = useSettings((state) => state.file_window_close);
  const update = useSettings((state) => state.update);
  return (
    <SectionCard title="窗口">
      <SettingRow
        title="关闭主窗口时"
        description="缩到托盘后会话、端口转发、AI 命令都继续运行，右键托盘图标选「退出」才真正退出。最小化仍然缩到任务栏。"
      >
        <Select
          value={closeAction}
          onValueChange={(value) => void update("close_action", parseCloseAction(value))}
        >
          <SelectTrigger className="h-9 w-56" data-testid="close-action">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="tray">缩到托盘</SelectItem>
            <SelectItem value="quit">退出程序</SelectItem>
          </SelectContent>
        </Select>
      </SettingRow>
      <SettingRow
        title="关闭 Z-SFTP 窗口时"
        description="断开并关闭：窗口里的 SFTP 全部断开（有传输在跑会先问）。合并回主窗口：标签原样交回主窗口，连接和传输都不中断。单个标签上的 × 始终是断开。"
      >
        <Select
          value={fileWindowClose}
          onValueChange={(value) =>
            void update("file_window_close", parseFileWindowCloseAction(value))
          }
        >
          <SelectTrigger className="h-9 w-56" data-testid="file-window-close">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="close">断开并关闭</SelectItem>
            <SelectItem value="merge">合并回主窗口</SelectItem>
          </SelectContent>
        </Select>
      </SettingRow>
    </SectionCard>
  );
}

/**
 * 主机树（UI-16）：不显示「未分组」文件夹，不分组的主机直接放在最下面。
 * 显示的是有效值（新用户的本机默认也算，UI-18）；点开关是用户的明确选择，写 vault_ungrouped_flat
 * （和主机树里「重命名「未分组」」同一项，跟着配置同步），关掉也会真的写下 false。
 */
function HostTreeCard() {
  const flat = useUngroupedFlat();
  const setUngroupedFlat = useSettings((state) => state.setUngroupedFlat);
  return (
    <SectionCard title="主机树">
      <SettingRow
        title="主机树里不显示「未分组」文件夹，不分组的主机直接放在最下面"
        description="打开后，没有分组的主机排在所有分组下面，中间用一条线隔开。"
      >
        <Switch
          data-testid="ungrouped-flat"
          checked={flat}
          onCheckedChange={(checked) => void setUngroupedFlat(checked)}
        />
      </SettingRow>
    </SectionCard>
  );
}

function AppearanceSection() {
  return (
    <>
      <SectionCard title="主题" description="跟随系统时，Windows 切换浅色 / 深色模式，界面跟着变。">
        <SettingRow title="主题">
          <ThemeModeButtons />
        </SettingRow>
      </SectionCard>
      <SectionCard
        title="应用皮肤"
        description="浅色和深色皮肤分别保存，随明暗模式生效。终端配色默认是 Tokyo Night，也可以改成跟随皮肤或固定为某一套，单独选择的终端配色会保留。"
      >
        <p className="pb-2 text-xs font-medium text-muted-foreground">浅色皮肤</p>
        <SkinGrid mode="light" />
        <p className="pb-2 pt-4 text-xs font-medium text-muted-foreground">深色皮肤</p>
        <SkinGrid mode="dark" />
      </SectionCard>
      <SectionCard title="终端配色">
        <SettingRow title="终端配色" description="跟随皮肤时，换皮肤终端也跟着换。">
          <TerminalThemeSelect />
        </SettingRow>
        <TerminalMismatchHint />
      </SectionCard>
      <HostTreeCard />
      <WindowCard />
    </>
  );
}

const COMMON_MONO_FONTS = [
  "JetBrains Mono",
  "Cascadia Mono",
  "Cascadia Code",
  "Consolas",
  "Courier New",
  "Lucida Console",
  "Source Code Pro",
  "Fira Code",
];

function TerminalSection() {
  const settings = useSettings();
  const update = useSettings((state) => state.update);
  // 字体：常见等宽字体里选，选"自定义"就手填一串字体栈
  const [customFont, setCustomFont] = useState(false);
  const known = COMMON_MONO_FONTS.some((font) =>
    settings.terminal_font_family.startsWith(font),
  );

  return (
    <>
      <SectionCard title="终端" description="已经打开的终端也会立刻跟着变。">
        <SettingRow
          title="字体"
          description="从常见等宽字体里选；需要别的字体栈就选「自定义」自己填。"
          stacked
        >
          <div className="space-y-2">
            <Select
              value={customFont || !known ? "__custom" : settings.terminal_font_family.split(",")[0]}
              onValueChange={(value) => {
                if (value === "__custom") {
                  setCustomFont(true);
                  return;
                }
                setCustomFont(false);
                void update(
                  "terminal_font_family",
                  `${value}, Cascadia Mono, Consolas, monospace`,
                );
              }}
            >
              <SelectTrigger className="h-9 w-72">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {COMMON_MONO_FONTS.map((font) => (
                  <SelectItem key={font} value={font}>
                    {font}
                  </SelectItem>
                ))}
                <SelectItem value="__custom">自定义…</SelectItem>
              </SelectContent>
            </Select>
            {(customFont || !known) && (
              <Input
                className="h-9 w-full font-mono text-xs"
                value={settings.terminal_font_family}
                spellCheck={false}
                onChange={(event) => void update("terminal_font_family", event.target.value)}
              />
            )}
          </div>
        </SettingRow>
        <SettingRow title="字号" description="10～24">
          <Slider
            value={settings.terminal_font_size}
            min={10}
            max={24}
            suffix="px"
            onChange={(value) => void update("terminal_font_size", value)}
          />
        </SettingRow>
        <SettingRow title="行高" description="1.0～1.6">
          <Slider
            value={settings.terminal_line_height}
            min={1}
            max={1.6}
            step={0.05}
            onChange={(value) => void update("terminal_line_height", value)}
          />
        </SettingRow>
        <SettingRow title="回滚行数" description="1000～100000">
          <Slider
            value={settings.terminal_scrollback}
            min={1000}
            max={100000}
            step={1000}
            onChange={(value) => void update("terminal_scrollback", value)}
          />
        </SettingRow>
        <SettingRow title="光标样式">
          <Select
            value={settings.terminal_cursor_style}
            onValueChange={(value) =>
              void update("terminal_cursor_style", value as "block" | "underline" | "bar")
            }
          >
            <SelectTrigger className="h-9 w-40">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="block">块</SelectItem>
              <SelectItem value="underline">下划线</SelectItem>
              <SelectItem value="bar">竖线</SelectItem>
            </SelectContent>
          </Select>
        </SettingRow>
        <SettingRow title="光标闪烁">
          <Switch
            checked={settings.terminal_cursor_blink}
            onCheckedChange={(checked) => void update("terminal_cursor_blink", checked)}
          />
        </SettingRow>
        <SettingRow
          title="终端里自动切到英文输入"
          description="焦点进终端时关闭输入法（直接打英文），离开终端回到其他输入框（AI 对话框、搜索框、编辑器等）时重新打开；要在终端里打中文，按 Ctrl+空格 或点输入法图标打开。微软拼音、百度拼音等都支持。"
        >
          <Switch
            checked={settings.terminal_ime_english}
            onCheckedChange={(checked) => void update("terminal_ime_english", checked)}
          />
        </SettingRow>
        <SettingRow title="选中即复制" description="选中文字之后自动复制到剪贴板。">
          <Switch
            checked={settings.terminal_copy_on_select}
            onCheckedChange={(checked) => void update("terminal_copy_on_select", checked)}
          />
        </SettingRow>
        <SettingRow title="右键粘贴" description="在终端里点右键直接粘贴剪贴板内容；要清屏时按住 Shift 点右键打开菜单。">
          <Switch
            checked={settings.terminal_right_click_paste}
            onCheckedChange={(checked) => void update("terminal_right_click_paste", checked)}
          />
        </SettingRow>
        <SettingRow
          title="多行粘贴前确认"
          description="粘贴的内容带换行时先问一下：对方 shell 没开括号粘贴模式时，换行会让命令直接执行。"
        >
          <Switch
            checked={settings.terminal_confirm_multiline_paste}
            onCheckedChange={(checked) =>
              void update("terminal_confirm_multiline_paste", checked)
            }
          />
        </SettingRow>
      </SectionCard>
      <HighlightSection />
      <SessionLogSection />
    </>
  );
}

/** SFTP（M3-2 第 5 节）：并发数、保留属性、默认显示隐藏文件。 */
function SftpSection() {
  const settings = useSettings();
  const update = useSettings((state) => state.update);

  return (
    <SectionCard
      title="SFTP"
      description="传输参数对之后打开的 SFTP 标签生效（正在传的任务不受影响）。"
    >
      <SettingRow title="同时传输的文件数" description="1～8，默认 3">
        <Slider
          value={settings.sftp_concurrency}
          min={1}
          max={8}
          suffix=" 个"
          onChange={(value) => void update("sftp_concurrency", value)}
        />
      </SettingRow>
      <SettingRow
        title="保留修改时间"
        description="传完之后把源文件的修改时间设到目标上（远端走 setstat，本地按文件时间）。"
      >
        <Switch
          checked={settings.sftp_preserve_mtime}
          onCheckedChange={(checked) => void update("sftp_preserve_mtime", checked)}
        />
      </SettingRow>
      <SettingRow title="保留权限" description="远端按源的权限位设置；本地只有只读一位有意义。">
        <Switch
          checked={settings.sftp_preserve_permissions}
          onCheckedChange={(checked) => void update("sftp_preserve_permissions", checked)}
        />
      </SettingRow>
      <SettingRow title="默认显示隐藏文件" description="两栏的初始开关；每个目录的隐藏文件按系统属性（远端按名字加点）判断。">
        <Switch
          checked={settings.sftp_show_hidden}
          onCheckedChange={(checked) => void update("sftp_show_hidden", checked)}
        />
      </SettingRow>
    </SectionCard>
  );
}

function ConnectionSection({
  onManageProxies,
  proxyPanelOpen,
}: {
  onManageProxies?: () => void;
  proxyPanelOpen: boolean;
}) {
  const settings = useSettings();
  const update = useSettings((state) => state.update);
  const proxies = useVault((state) => state.proxies);
  const [defaultProxyId, setDefaultProxyId] = useState<string>("none");
  const [appProxyRaw, setAppProxyRaw] = useState<string | null>(null);
  const [systemProxy, setSystemProxy] = useState<SystemProxy | null>(null);
  const appProxy = appProxyChoice(
    appProxyRaw,
    proxies.map((proxy) => proxy.id),
  );

  useEffect(() => {
    let cancelled = false;
    void settingsGet(APP_NETWORK_PROXY_KEY)
      .then((raw) => {
        if (!cancelled) setAppProxyRaw(raw);
      })
      .catch((error: unknown) => console.error("读软件网络代理失败", error));
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (appProxy !== "system") return;
    let cancelled = false;
    void appNetworkSystemProxy()
      .then((proxy) => {
        if (!cancelled) setSystemProxy(proxy);
      })
      .catch((error: unknown) => console.error("读系统代理失败", error));
    return () => {
      cancelled = true;
    };
  }, [appProxy]);

  useEffect(() => {
    let cancelled = false;
    void defaultProxyGet()
      .then((id) => {
        if (!cancelled) setDefaultProxyId(id === null ? "none" : String(id));
      })
      .catch((error: unknown) => console.error("读默认代理失败", error));
    return () => {
      cancelled = true;
    };
    // 代理面板开关时重读：用户可能在面板里换了默认代理，这里不能停在旧值
  }, [proxyPanelOpen]);

  return (
    <>
      <SectionCard title="连接">
        <SettingRow
          title="SSH 默认代理"
          description="只管 SSH 连接：「跟随默认代理」的主机和快速连接走这一个。"
        >
          <Select
            value={defaultProxyId}
            onValueChange={(value) => {
              setDefaultProxyId(value);
              void defaultProxySet(value === "none" ? null : Number(value)).catch(
                (error: unknown) => toast.error(String(error)),
              );
            }}
          >
            <SelectTrigger className="h-9 w-56">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="none">不使用默认代理</SelectItem>
              {proxies.map((proxy) => (
                <SelectItem key={proxy.id} value={String(proxy.id)}>
                  {proxy.name}（{proxy.kind}）
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {/* 以前这里只有下拉框，代理列表为空时用户找不到在哪新建（R-11） */}
          {proxies.length === 0 && (
            <p className="mt-1 w-56 text-xs text-muted-foreground">
              还没有代理，点下面的「新建 / 管理代理」添加一个。
            </p>
          )}
          {onManageProxies && (
            <Button
              variant="outline"
              size="sm"
              className="mt-2 h-8 w-56 gap-1.5 text-xs"
              onClick={onManageProxies}
            >
              <Network size={13} />
              新建 / 管理代理
            </Button>
          )}
        </SettingRow>
        <SettingRow
          title="软件网络"
          description="检查和下载更新、配置同步、下载 opencode 走这里，和 SSH 的代理分开。"
        >
          <Select
            value={appProxy}
            onValueChange={(value) => {
              const raw = appProxyValue(value);
              setAppProxyRaw(raw);
              void settingsSet(APP_NETWORK_PROXY_KEY, raw).catch((error: unknown) =>
                toast.error(String(error)),
              );
            }}
          >
            <SelectTrigger className="h-9 w-56" data-testid="app-network-proxy">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="ssh_default">使用 SSH 默认代理</SelectItem>
              <SelectItem value="system">跟随系统代理</SelectItem>
              <SelectItem value="none">不使用代理</SelectItem>
              {proxies.map((proxy) => (
                <SelectItem key={proxy.id} value={`proxy:${proxy.id}`}>
                  {proxy.name}（{proxy.kind}）
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          {appProxy === "system" && (
            <p className="mt-1 w-56 text-xs text-muted-foreground">
              {systemProxyText(systemProxy)}
            </p>
          )}
        </SettingRow>
        <SettingRow
          title="连接空闲断开"
          description="最后一个终端关掉之后，连接再空转多久断开；0 = 立刻断开。改完对之后进入空闲的连接生效。"
        >
          <Slider
            value={settings.pool_idle_timeout_secs}
            min={0}
            max={3600}
            step={10}
            suffix="s"
            onChange={(value) => void update("pool_idle_timeout_secs", value)}
          />
        </SettingRow>
        <SettingRow
          title="保活间隔"
          description="多久发一次 keepalive 探测断线；0 = 关闭。"
        >
          <Slider
            value={settings.keepalive_interval_secs}
            min={0}
            max={300}
            step={5}
            suffix="s"
            onChange={(value) => void update("keepalive_interval_secs", value)}
          />
        </SettingRow>
        <SettingRow
          title="显示服务器状态栏"
          description="连上后在终端底部显示 CPU / 内存 / 磁盘 / 网速。"
        >
          <Switch
            checked={settings.stats_show}
            onCheckedChange={(checked) => void update("stats_show", checked)}
          />
        </SettingRow>
        <SettingRow
          title="状态栏刷新间隔"
          description="多久采一次服务器状态；状态栏关掉时这项不起作用。"
        >
          <Slider
            value={settings.stats_refresh_secs}
            min={1}
            max={30}
            suffix="s"
            onChange={(value) => void update("stats_refresh_secs", value)}
          />
        </SettingRow>
      </SectionCard>
    </>
  );
}

const SHORTCUTS: { keys: string; what: string }[] = [
  { keys: "Ctrl+Shift+T", what: "新标签" },
  { keys: "Ctrl+Shift+W", what: "关闭当前 pane（只剩一个时关掉整个标签）" },
  { keys: "Ctrl+Shift+D", what: "左右分屏（复用连接）" },
  { keys: "Ctrl+Shift+E", what: "上下分屏" },
  { keys: "Ctrl+Tab / Ctrl+Shift+Tab", what: "切换标签" },
  { keys: "Ctrl+,", what: "打开设置" },
  { keys: "R", what: "断线的 pane 上按 R 重连" },
];

function ShortcutsSection() {
  return (
    <SectionCard title="快捷键" description="M1 不支持自定义（放在后续版本）。">
      <div className="divide-y divide-border/40">
        {SHORTCUTS.map((item) => (
          <div key={item.keys} className="flex items-center justify-between gap-6 py-2.5">
            <span className="text-[13px]">{item.what}</span>
            <span className="shrink-0 rounded border border-border/60 bg-secondary px-2 py-0.5 font-mono text-xs">
              {item.keys}
            </span>
          </div>
        ))}
      </div>
    </SectionCard>
  );
}

/** AI（AI-2b；AI-3 加 codex）：执行模式默认值、每个智能体的发现/登录/手动指定、opencode 一键下载。 */
export function AISection() {
  const execMode = useSettings((state) => state.ai_exec_mode);
  const defaultAgent = useSettings((state) => state.ai_default_agent);
  const backupKeep = useSettings((state) => state.ai_write_backup_keep);
  const update = useSettings((state) => state.update);
  const [downloading, setDownloading] = useState(false);
  // state 要等下一次渲染才变，连点两下时第二下看到的还是 false；用 ref 同步拦住重入。
  const downloadBusy = useRef(false);
  const [downloadError, setDownloadError] = useState<string | null>(null);
  // 下载成功后 +1，opencode 那一块据此重新检测（和"重新检测"按钮同一个入口）。
  const [opencodeRecheck, setOpencodeRecheck] = useState(0);
  const [progress, setProgress] = useState<{ received: number; total: number } | null>(null);
  // 下载前先弹确认框（来源 + 大小）。
  const [confirm, setConfirm] = useState<ReleaseInfo | null>(null);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void onDownloadProgress((p) => {
      if (!p.done) setProgress({ received: p.received, total: p.total });
    }).then((stop) => {
      if (cancelled) stop();
      else unlisten = stop;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const finishDownload = () => {
    downloadBusy.current = false;
    setDownloading(false);
    setProgress(null);
  };

  const doDownload = () => {
    setConfirm(null);
    setProgress(null);
    void downloadAndRecheck(aiAgentDownload, () => setOpencodeRecheck((n) => n + 1))
      .then((outcome) => {
        if (outcome.ok) {
          toast.success(`已下载 opencode ${outcome.install.version}`);
        } else {
          setDownloadError(outcome.error);
          toast.error(outcome.error);
        }
      })
      .finally(finishDownload);
  };

  const startDownload = () => {
    if (downloadBusy.current) return;
    downloadBusy.current = true;
    setDownloading(true);
    setDownloadError(null);
    void aiAgentDownloadInfo()
      .then((info) => setConfirm(info))
      .catch((error: unknown) => {
        const text = errorText(error);
        setDownloadError(text);
        toast.error(text);
        finishDownload();
      });
  };

  // 确认框里点了取消（或关掉）：没开始下载，按钮要恢复，不然一直显示"下载中…"。
  const cancelConfirm = () => {
    setConfirm(null);
    finishDownload();
  };

  return (
    <SectionCard title="AI" description="AI 助手通过外部智能体在服务器上执行命令，遵守下面的执行模式。">
      <SettingRow
        title="默认智能体"
        description="打开 AI 面板时默认用哪一个（面板顶部还能按对话切换）。"
      >
        <Select
          value={defaultAgent}
          onValueChange={(value) =>
            void update(
              "ai_default_agent",
              value === "codex" ? "codex" : value === "claude" ? "claude" : "opencode",
            )
          }
        >
          <SelectTrigger className="w-32">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="opencode">opencode</SelectItem>
            <SelectItem value="codex">codex</SelectItem>
            <SelectItem value="claude">Claude Code</SelectItem>
          </SelectContent>
        </Select>
      </SettingRow>
      <SettingRow
        title="执行模式"
        description={EXEC_MODE_OPTIONS.map((item) => `${item.label}：${item.hint}`).join(" ")}
      >
        <Select
          value={toExecMode(execMode)}
          onValueChange={(value) => void update("ai_exec_mode", toExecMode(value))}
        >
          <SelectTrigger className="w-32" data-section="settings-exec-mode">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {EXEC_MODE_OPTIONS.map((item) => (
              <SelectItem key={item.value} value={item.value}>
                {item.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </SettingRow>
      <SettingRow
        title="写文件备份份数"
        description="AI 覆盖服务器文件时，每个文件只保留最近几份 .zssh-bak-* 备份，更早的删掉。"
      >
        <Slider
          value={backupKeep}
          min={1}
          max={20}
          suffix=" 份"
          onChange={(value) => void update("ai_write_backup_keep", value)}
        />
      </SettingRow>
      <ClearAiHistoryRow />
      <AgentBlock
        agent="opencode"
        description="发现方式：自动按 数据目录 tools → PATH → 常见安装位置 的顺序找；手动只用指定路径（坏了就报错，不回退）。"
        pathKey="ai_opencode_path"
        recheckSignal={opencodeRecheck}
        placeholder={"C:\\path\\to\\opencode.exe"}
        renderExtra={
          <Button size="sm" disabled={downloading} onClick={startDownload}>
            {downloading ? "下载中…" : "下载 opencode"}
          </Button>
        }
      />
      {downloading && progress && progress.total > 0 && (
        <SettingRow title="下载进度" stacked>
          <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
            <div
              className="h-full bg-primary transition-all"
              style={{ width: `${Math.round((progress.received / progress.total) * 100)}%` }}
            />
          </div>
        </SettingRow>
      )}
      {downloadError && !downloading && (
        <p className="text-xs text-destructive" data-section="opencode-download-error">
          下载失败：{downloadError}
        </p>
      )}
      <AgentBlock
        agent="codex"
        description="codex 不提供一键下载：先 `npm i -g @openai/codex`，或在这里手动指定路径。发现方式同上；模型和登录用 codex 自己的配置。"
        pathKey="ai_codex_path"
        placeholder={"C:\\path\\to\\codex.exe"}
      />
      <AgentBlock
        agent="claude"
        description="Claude Code 不提供一键下载：先 `npm i -g @anthropic-ai/claude-code`（或官方安装脚本，装到 ~/.local/bin），也可以在这里手动指定路径。发现方式同上；登录和模型用 Claude Code 自己的配置（不改 CLAUDE_CONFIG_DIR / HOME）。"
        pathKey="ai_claude_path"
        placeholder={"C:\\path\\to\\claude.exe"}
      />

      {/* 下载确认：来源 + 大小（卡片 §4）。 */}
      <Dialog open={confirm !== null} onOpenChange={(open) => !open && cancelConfirm()}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>下载 opencode</DialogTitle>
          </DialogHeader>
          {confirm && (
            <div className="space-y-2 text-sm">
              <p>来源：</p>
              <p className="break-all font-mono text-xs text-muted-foreground">
                {confirm.page_url || confirm.download_url}
              </p>
              <p>
                版本：<span className="font-mono">{confirm.version}</span>
              </p>
              <p>
                大小：<span className="font-mono">{formatBytes(confirm.size)}</span>
              </p>
              <div className="flex justify-end gap-2 pt-2">
                <Button variant="outline" size="sm" onClick={cancelConfirm}>
                  取消
                </Button>
                <Button size="sm" onClick={doDownload}>
                  开始下载
                </Button>
              </div>
            </div>
          )}
        </DialogContent>
      </Dialog>
    </SectionCard>
  );
}

/** 登录状态文案：`null` = 读不了配置，判断不了（R-2 第 3 节：不阻断，只提示）。 */
function loginText(loggedIn: boolean | null): string {
  if (loggedIn === true) return "已登录";
  if (loggedIn === false) return "未登录";
  return "不确定";
}

/** 一个智能体的发现/登录/手动指定块（opencode 和 codex 共用，AI-3）。 */
function AgentBlock({
  agent,
  description,
  pathKey,
  placeholder,
  renderExtra,
  recheckSignal,
}: {
  agent: AgentId;
  description: string;
  pathKey: "ai_opencode_path" | "ai_codex_path" | "ai_claude_path";
  placeholder: string;
  renderExtra?: React.ReactNode;
  /** 变化时重新检测（opencode 下载完成后由父组件递增）；首次渲染也会检测一次。 */
  recheckSignal?: number;
}) {
  const path = useSettings((state) => state[pathKey]);
  const label = agent === "claude" ? "Claude Code" : agent;
  const update = useSettings((state) => state.update);
  const [detected, setDetected] = useState<AgentInstall | null | undefined>(undefined);
  const [login, setLogin] = useState<AgentLoginStatus | null>(null);
  /** codex 的本机工具禁用核对（UI-1 0.1）：非空 = 拒绝启动的完整原因。 */
  const [guard, setGuard] = useState<string | null>(null);
  const [manual, setManual] = useState(path);
  const [probing, setProbing] = useState(false);
  /**
   * 开关拨到「手动」但还没保存路径：先显示输入框，点「检测并保存」通过了才存。
   * 以前手动模式只看「存没存过路径」，没存过时开关拨不过去、输入框也出不来（issue #3）。
   */
  const [manualPending, setManualPending] = useState(false);
  /** 每次「检测并保存」和每次拨开关都加一：检测结果回来时序号对不上（中途切回了自动）就作废。 */
  const probeSeq = useRef(0);

  // 设置读回来（或别处改了）路径：输入框跟着显示存着的值。手动还没保存好时不动输入框
  // （保存失败、设置回滚成空时，草稿要留着）。
  useEffect(() => {
    if (!manualPending) setManual(path);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path]);

  // 存了手动路径，或者刚拨到「手动」还没保存，都算手动模式；留空是自动（默认）。
  const isManual = path.trim() !== "" || manualPending;

  const refresh = () => {
    setDetected(undefined);
    setGuard(null);
    void aiAgentDiscover(agent)
      .then((found) => {
        setDetected(found);
        if (agent !== "opencode" && found && !found.too_old) {
          void aiAgentLogin(agent).then(setLogin).catch(() => {});
        }
        if (agent === "codex" && found && !found.too_old) {
          void aiAgentGuard("codex", found.path).then(setGuard).catch(() => {});
        }
      })
      .catch((error: unknown) => {
        toast.error(String(error));
        setDetected(null);
      });
  };

  useEffect(() => {
    refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [recheckSignal]);

  const detectedLine =
    detected === undefined
      ? "检测中…"
      : detected === null
        ? isManual
          ? "手动路径不可用"
          : "未找到"
        : `${detected.version}（${sourceLabel(agent, detected.source)}）`;

  return (
    <SettingRow
      title={label}
      description={`当前：${detectedLine}${agent !== "opencode" && login ? `；登录：${loginText(login.logged_in)}` : ""}`}
      stacked
    >
      <p className="text-xs text-muted-foreground">{description}</p>
      {/* 按钮行和上面的说明文字隔开一点，不要贴在一起 */}
      <div className="mt-2.5 flex items-center gap-2">
        <Button variant="outline" size="sm" disabled={probing} onClick={refresh}>
          {probing ? "检测中…" : "重新检测"}
        </Button>
        {renderExtra}
        <div className="ml-auto flex items-center gap-1.5 text-xs">
          <span className={isManual ? "text-muted-foreground" : "font-medium"}>自动</span>
          <Switch
            checked={isManual}
            aria-label={`${label} 手动指定路径`}
            onCheckedChange={(checked) => {
              probeSeq.current += 1;
              if (checked) {
                // 拨到手动：先出输入框，检测通过才保存
                setManualPending(true);
                return;
              }
              // 切回自动就清空手动路径
              setManualPending(false);
              setManual("");
              if (path.trim() !== "") void update(pathKey, "").then(refresh);
            }}
          />
          <span className={isManual ? "font-medium" : "text-muted-foreground"}>手动指定</span>
        </div>
      </div>
      {detected && !detected.auto && (
        <p className="mt-1 font-mono text-[11px] text-muted-foreground">{detected.path}</p>
      )}
      {detected?.auto && (
        <p className="mt-1 text-xs text-muted-foreground">
          已自动发现：<span className="font-mono">{detected.path}</span>（{detected.version}）
        </p>
      )}
      {detected && !detected.too_old && skippedOlderNote(detected) && (
        <p className="mt-1 text-xs text-muted-foreground" data-section="skipped-older">
          {skippedOlderNote(detected)}
        </p>
      )}
      {detected?.too_old && (
        <p className="mt-1 flex items-center gap-1.5 text-xs text-amber-600">
          版本过低，建议升级
          {agent === "opencode"
            ? "（低于 1.19 在共享数据目录上有已知问题）"
            : agent === "codex"
              ? "（codex 需要 0.144.0 及以上，可 `codex update`）"
              : "（Claude Code 需要 2.0.41 及以上，可 `claude update`）"}。
        </p>
      )}
      {agent !== "opencode" && login && login.logged_in !== true && (
        <p className="mt-1 text-xs text-amber-600">
          {login.logged_in === false ? "未登录" : "登录状态不确定"}
          {login.hint
            ? `：${login.hint}`
            : agent === "codex"
              ? "：请先在终端里运行 `codex login`"
              : "：请先在终端里运行 `claude` 完成登录"}
        </p>
      )}
      {guard && (
        <p className="mt-1 text-xs text-destructive" data-section="codex-guard">
          {guard}（点「重新检测」重试）
        </p>
      )}
      {isManual && (
        <div className="mt-2 flex items-center gap-2">
          <Input
            value={manual}
            onChange={(event) => setManual(event.target.value)}
            placeholder={placeholder}
            className="h-9 flex-1 font-mono text-xs"
          />
          <Button
            variant="outline"
            size="sm"
            disabled={probing}
            onClick={() => {
              const seq = (probeSeq.current += 1);
              const value = manual;
              setProbing(true);
              void aiAgentProbePath(agent, value)
                .then((install) => {
                  // 检测期间切回了自动（或又点了一次）：这次结果作废，别把路径又存回去
                  if (seq !== probeSeq.current) return;
                  toast.success(`可用：${install.version}`);
                  // 保存结束前都算草稿（第一次填、改已有路径都一样）：失败时设置回滚，输入框不跟着被冲掉
                  setManualPending(true);
                  void update(pathKey, value).then((saved) => {
                    // 保存成功才收起「待保存」；失败时设置会回滚成原值，输入框和草稿留着
                    if (saved && seq === probeSeq.current) setManualPending(false);
                    refresh();
                  });
                })
                .catch((error: unknown) => {
                  if (seq === probeSeq.current) toast.error(String(error));
                })
                .finally(() => setProbing(false));
            }}
          >
            检测并保存
          </Button>
        </div>
      )}
    </SettingRow>
  );
}

function formatBytes(bytes: number): string {
  if (bytes <= 0) return "未知";
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/**
 * 安装来源的显示名（UI-1 0.3）。
 *
 * `tools_dir` 对 opencode 才是"已下载"（Z-SSH 一键下载装在那）；codex 不提供下载，
 * 落在 tools 目录的只可能是用户自己放的，显示"已找到"。
 */
function sourceLabel(agent: AgentId, source: AgentInstall["source"]): string {
  switch (source) {
    case "setting":
      return "手动指定";
    case "tools_dir":
      return agent === "opencode" ? "已下载" : "已找到";
    case "known_location":
      return "常见安装位置";
    default:
      return "PATH";
  }
}

/** 关于页里的更新：当前状态、检查更新按钮、启动时自动检查的开关。 */
function UpdateRow() {
  const status = useUpdater((state) => state.status);
  const prefs = useUpdater((state) => state.prefs);
  const checkManually = useUpdater((state) => state.checkManually);
  const openDialog = useUpdater((state) => state.openDialog);
  const setAuto = useUpdater((state) => state.setAuto);
  const download = useUpdater((state) => state.download);
  const portable = useUpdater((state) => state.portable);
  const openDownloadPage = useUpdater((state) => state.openDownloadPage);
  const busy = status?.phase === "checking" || status?.phase === "downloading";
  // 下载中的百分比和大小在下面的进度条里写，这一行不再重复
  const text =
    status?.phase === "downloading"
      ? "下载完会先校验签名，通过后在退出 Z-SSH 时自动安装。"
      : aboutStatusText(status, prefs?.auto_check ?? true);
  return (
    <>
      <SettingRow
        title="检查更新"
        description={text || "从 GitHub 发布页检查有没有新版本，走「连接」→「软件网络」里设置的代理。"}
      >
        <div className="flex items-center gap-2">
          {status?.phase === "available" && (
            <Button variant="outline" size="sm" onClick={() => openDialog(true)}>
              查看更新
            </Button>
          )}
          {portable &&
            (status?.phase === "available" ||
              (status?.phase === "failed" && status.stage === "download")) && (
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                void openDownloadPage().catch((error: unknown) => toast.error(String(error)));
              }}
            >
              打开下载页
            </Button>
          )}
          {!portable && status?.phase === "failed" && status.stage !== "check" && (
            <Button variant="outline" size="sm" onClick={() => void download()}>
              重试下载
            </Button>
          )}
          <Button
            variant="outline"
            size="sm"
            disabled={busy}
            onClick={() => {
              void checkManually().catch((error: unknown) => toast.error(String(error)));
            }}
          >
            <RefreshCw size={14} className={status?.phase === "checking" ? "animate-spin" : ""} />
            检查更新
          </Button>
        </div>
      </SettingRow>
      <UpdateProgress status={status} />
      <SettingRow
        title="启动时自动检查"
        description="每次打开 Z-SSH 时检查一次，程序开着时每 6 小时再查一次；有新版本会在后台下载，下好后弹窗询问，选「稍后」就在退出 Z-SSH 时自动安装，下次打开就是新版。关掉后只在点「检查更新」时检查。"
      >
        <Switch
          checked={prefs?.auto_check ?? true}
          onCheckedChange={(checked) => {
            void setAuto(checked).catch((error: unknown) => toast.error(String(error)));
          }}
        />
      </SettingRow>
    </>
  );
}

function AboutSection() {
  const [version, setVersion] = useState("…");
  const [paths, setPaths] = useState<{ data_dir: string; known_hosts: string } | null>(null);

  useEffect(() => {
    let cancelled = false;
    void getVersion()
      .then((value) => {
        if (!cancelled) setVersion(value);
      })
      .catch((error: unknown) => console.error("读版本失败", error));
    void dataDirInfo()
      .then((info) => {
        if (!cancelled) setPaths(info);
      })
      .catch((error: unknown) => console.error("读数据目录失败", error));
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <SectionCard title="关于">
      <SettingRow title="Z-SSH" description="面向 Windows 的 SSH 客户端：标签与分屏终端、跳板机与代理、AI 运维助手。">
        <span className="font-mono text-sm text-muted-foreground">v{version}</span>
      </SettingRow>
      <UpdateRow />
      <SettingRow title="数据目录" description="主机库、设置和已知主机指纹都在这里。" stacked>
        <div className="flex items-center gap-2">
          <Input readOnly value={paths?.data_dir ?? "…"} className="h-9 flex-1 font-mono text-xs" />
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              void openDataDir().catch((error: unknown) => toast.error(String(error)));
            }}
          >
            <FolderOpen size={14} /> 打开所在文件夹
          </Button>
        </div>
      </SettingRow>
      <SettingRow title="known_hosts" description="自己维护的指纹文件（系统那份只读）。" stacked>
        <Input readOnly value={paths?.known_hosts ?? "…"} className="h-9 font-mono text-xs" />
      </SettingRow>
      <SettingRow title="开源许可" description="Z-SSH 以 GPL-3.0-or-later 许可发布；第三方组件与致谢见第三方声明。">
        <Dialog>
          <DialogTrigger asChild>
            <Button variant="outline" size="sm">
              <FileText size={14} /> 第三方声明
            </Button>
          </DialogTrigger>
          <DialogContent className="max-w-2xl">
            <DialogHeader>
              <DialogTitle>第三方声明</DialogTitle>
            </DialogHeader>
            <pre className="max-h-[60vh] overflow-auto whitespace-pre-wrap rounded-md border bg-muted/40 p-3 font-mono text-xs leading-relaxed text-muted-foreground">
              {NOTICE_TEXT}
            </pre>
          </DialogContent>
        </Dialog>
      </SettingRow>
    </SectionCard>
  );
}
