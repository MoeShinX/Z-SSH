//! 设置项的读写（后端 settings 表，值是 JSON 文本）。

import { invoke } from "@tauri-apps/api/core";

export const settingsGet = (key: string): Promise<string | null> =>
  invoke("settings_get", { key });

export const settingsSet = (key: string, value: string): Promise<void> =>
  invoke("settings_set", { key, value });

/** 数据目录和 known_hosts 的路径（设置页的"关于"用）。 */
export interface DataDirInfo {
  data_dir: string;
  known_hosts: string;
}

export const dataDirInfo = (): Promise<DataDirInfo> => invoke("data_dir_info");

/** 用资源管理器打开数据目录。 */
export const openDataDir = (): Promise<void> => invoke("open_data_dir");

/** 用系统默认浏览器打开网址（后端只放行 http / https）。 */
export const openExternalUrl = (url: string): Promise<void> =>
  invoke("open_external_url", { url });

/** 会话日志实际写到的目录（设置没填时是数据目录下的 session-logs）。 */
export const sessionLogDirInfo = (): Promise<string> => invoke("session_log_dir_info");

/** 用资源管理器打开会话日志目录。 */
export const openSessionLogDir = (): Promise<void> => invoke("open_session_log_dir");

/** Windows 系统代理（Internet 选项里的设置）。 */
export type SystemProxy =
  | { kind: "none" }
  | { kind: "manual"; scheme: string; host: string; port: number }
  | { kind: "auto_config"; url: string };

export const appNetworkSystemProxy = (): Promise<SystemProxy> =>
  invoke("app_network_system_proxy");
