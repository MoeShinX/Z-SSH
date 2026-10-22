//! 往终端输入行里插路径时的引号规则（M3-3 第 3、4 节）：只插入，不执行（不加回车）。
//!
//! 引号规则看的是**接收端**：这些字节是由远端 POSIX shell 来解析的，不管路径本来是
//! 远端的还是 Windows 本地的，都按单引号规则引用。双引号里 shell 照样展开 `$()`、
//! 反引号和 `$变量`，不能用来装不可信的文件名。

const SAFE_REMOTE = /^[A-Za-z0-9_/.,:@%+=~-]+$/;

/** 单引号整体包起来：里面除了 `'` 以外一切原样（含反斜杠）；`'` 写成 `'\''`（闭合、转义、再开）。 */
function singleQuote(path: string): string {
  // 替换串写成函数返回值：字符串里的 `$'` 之类在 replace 里有特殊含义，这样不用操心
  return `'${path.replace(/'/g, () => "'\\''")}'`;
}

/** 远端路径给 POSIX shell 用：安全字符原样，其余用单引号包起来（`'` → `'\''`）。 */
export function quoteRemotePath(path: string): string {
  if (SAFE_REMOTE.test(path)) return path;
  return singleQuote(path);
}

/**
 * 本地（Windows）路径插进远端终端：同样用 POSIX 单引号规则，一律加引号。
 * Windows 文件名里 `$ ( ) ` ' ;` 都合法，双引号挡不住它们；反斜杠在单引号里原样保留。
 */
export function quoteLocalPath(path: string): string {
  return singleQuote(path);
}

/**
 * 路径里有没有终端控制字符（C0 `\x00`–`\x1f`，含换行、回车、ESC，以及 `\x7f`）。
 * 单引号只管 shell 的语法，终端行编辑在 shell 解析之前就会先处理这些字符，
 * 所以带它们的文件名不能往终端里插，引用救不了。
 */
export function hasControlChars(text: string): boolean {
  return /[\x00-\x1f\x7f]/.test(text);
}

/** 多个路径用空格隔开，不加回车、不加结尾空格。 */
export function joinQuoted(paths: string[], quote: (path: string) => string): string {
  return paths.map(quote).join(" ");
}
