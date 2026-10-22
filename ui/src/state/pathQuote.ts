//! 往终端输入行里插路径时的引号规则（M3-3 第 3、4 节）：只插入，不执行（不加回车）。

const SAFE_REMOTE = /^[A-Za-z0-9_/.,:@%+=~-]+$/;

/** 远端路径给 POSIX shell 用：安全字符原样，其余用单引号包起来（`'` → `'\''`）。 */
export function quoteRemotePath(path: string): string {
  if (SAFE_REMOTE.test(path)) return path;
  return `'${path.replace(/'/g, `'\''`)}'`;
}

/** 本地（Windows）路径：一律加双引号，里面的双引号转成 `\"` 之外不再处理。 */
export function quoteLocalPath(path: string): string {
  return `"${path.replace(/"/g, '\\"')}"`;
}

/** 多个路径用空格隔开，不加回车、不加结尾空格。 */
export function joinQuoted(paths: string[], quote: (path: string) => string): string {
  return paths.map(quote).join(" ");
}
