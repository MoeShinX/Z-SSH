//! 内置编辑器按文件名判断语法高亮用哪种语言（M3-5 第 2 节）。纯函数，认不出就是纯文本。

export type LanguageId =
  | "shell"
  | "properties"
  | "yaml"
  | "json"
  | "xml"
  | "html"
  | "nginx"
  | "python"
  | "javascript"
  | "typescript"
  | "sql"
  | "dockerfile"
  | "markdown"
  | "toml"
  | "css";

/** 显示在状态栏上的名字。 */
export const LANGUAGE_NAMES: Record<LanguageId | "text", string> = {
  text: "纯文本",
  shell: "Shell",
  properties: "配置文件",
  yaml: "YAML",
  json: "JSON",
  xml: "XML",
  html: "HTML",
  nginx: "Nginx",
  python: "Python",
  javascript: "JavaScript",
  typescript: "TypeScript",
  sql: "SQL",
  dockerfile: "Dockerfile",
  markdown: "Markdown",
  toml: "TOML",
  css: "CSS",
};

const BY_EXTENSION: Record<string, LanguageId> = {
  sh: "shell",
  bash: "shell",
  zsh: "shell",
  ksh: "shell",
  fish: "shell",
  conf: "properties",
  ini: "properties",
  cfg: "properties",
  cnf: "properties",
  properties: "properties",
  env: "properties",
  service: "properties",
  socket: "properties",
  timer: "properties",
  desktop: "properties",
  repo: "properties",
  yaml: "yaml",
  yml: "yaml",
  json: "json",
  jsonc: "json",
  xml: "xml",
  svg: "xml",
  plist: "xml",
  xsl: "xml",
  html: "html",
  htm: "html",
  py: "python",
  pyw: "python",
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  jsx: "javascript",
  ts: "typescript",
  tsx: "typescript",
  sql: "sql",
  md: "markdown",
  markdown: "markdown",
  toml: "toml",
  css: "css",
};

/** 整个文件名就能认出来的（没有扩展名，或者扩展名不说明问题）。 */
const BY_NAME: Record<string, LanguageId> = {
  dockerfile: "dockerfile",
  containerfile: "dockerfile",
  "nginx.conf": "nginx",
  ".bashrc": "shell",
  ".bash_profile": "shell",
  ".bash_aliases": "shell",
  ".bash_logout": "shell",
  ".profile": "shell",
  ".zshrc": "shell",
  ".zprofile": "shell",
  ".zshenv": "shell",
  ".kshrc": "shell",
  profile: "shell",
  bashrc: "shell",
  "crontab": "shell",
  "authorized_keys": "properties",
  "sshd_config": "properties",
  "ssh_config": "properties",
  "fstab": "properties",
  "hosts": "properties",
  "resolv.conf": "properties",
  ".gitconfig": "properties",
  ".editorconfig": "properties",
  ".env": "properties",
  "pipfile": "toml",
  "cargo.lock": "toml",
};

function baseName(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  const cut = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return cut < 0 ? trimmed : trimmed.slice(cut + 1);
}

/** 按文件名（可以带路径）判断语言；认不出返回 `text`。 */
export function languageFor(path: string): LanguageId | "text" {
  const name = baseName(path);
  const lower = name.toLowerCase();
  if (BY_NAME[lower]) return BY_NAME[lower];
  // Dockerfile.dev、dev.Dockerfile
  if (lower.startsWith("dockerfile.") || lower.endsWith(".dockerfile")) return "dockerfile";
  const dot = lower.lastIndexOf(".");
  const extension = dot > 0 ? lower.slice(dot + 1) : "";
  // nginx 的配置：`.nginx`，或者 nginx 目录里没有扩展名 / 扩展名是 .conf 的文件
  //（/etc/nginx/sites-available/default、/etc/nginx/conf.d/site.conf）。
  if (extension === "nginx") return "nginx";
  const normalized = path.replace(/\\/g, "/").toLowerCase();
  if (/(^|\/)nginx\//.test(normalized) && (extension === "conf" || extension === "")) {
    return "nginx";
  }
  if (extension && BY_EXTENSION[extension]) return BY_EXTENSION[extension];
  // 没有扩展名：看开头的 shebang 要读内容，这里不做；认不出就是纯文本。
  return "text";
}

/** 看第一行的 shebang 补一刀（`#!/bin/bash`、`#!/usr/bin/env python3`）。 */
export function languageFromShebang(firstLine: string): LanguageId | null {
  if (!firstLine.startsWith("#!")) return null;
  const line = firstLine.toLowerCase();
  if (/\b(ba|z|k|da|a)?sh\b/.test(line)) return "shell";
  if (/\bpython[0-9.]*\b/.test(line)) return "python";
  if (/\b(node|nodejs|deno|bun)\b/.test(line)) return "javascript";
  return null;
}
