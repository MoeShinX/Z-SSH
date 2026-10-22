//! AI 回复的 Markdown 排版（AI-UX-1 第 3 项）：GitHub 风格（标题、加粗、列表、表格、
//! 行内代码、代码块、引用、分隔线）。用户自己发的消息不走这里，仍是纯文本。
//!
//! AI 的输出里可能夹带服务器上的提示注入内容，所以：
//! - 不渲染原始 HTML（没有挂 rehype-raw，`<img onerror>`、`<script>` 之类按文字显示）；
//! - 不加载任何图片（图片位置显示成"[图片：说明]"文字，不发请求）；
//! - 链接不是 `<a>`：显示链接文字和完整网址，点击只复制网址，不跳转、不开浏览器。

import { Copy } from "lucide-react";
import { createContext, memo, useContext, type ReactNode } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";

import { toast } from "../ui/toast";

/** 取一段 React 子节点里的纯文字（复制代码块、比较链接文字用）。 */
function textOf(node: ReactNode): string {
  if (node === null || node === undefined || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(textOf).join("");
  if (typeof node === "object" && "props" in node) {
    return textOf((node.props as { children?: ReactNode }).children);
  }
  return "";
}

async function copyText(text: string, doneMessage: string) {
  try {
    await navigator.clipboard.writeText(text);
    toast.success(doneMessage);
  } catch {
    toast.error("复制失败");
  }
}

/** 正在 `<pre>` 里面：里面的 `<code>` 是代码块的内容，不套行内代码的样式。 */
const InPreContext = createContext(false);

function CodeBlock({ children }: { children?: ReactNode }) {
  // fenced 代码块的文字末尾带一个换行，复制时去掉
  const text = textOf(children).replace(/\n$/, "");
  return (
    <div data-section="md-code-block" className="group/code relative my-2">
      <button
        type="button"
        data-action="copy-code"
        title="复制代码"
        onClick={() => void copyText(text, "已复制代码")}
        className="absolute right-1 top-1 z-10 flex items-center gap-1 rounded border border-border/60 bg-background/80 px-1.5 py-0.5 text-[10px] text-muted-foreground hover:text-foreground"
      >
        <Copy size={10} />
        复制
      </button>
      <pre className="overflow-x-auto rounded-md border border-border/60 bg-muted/50 p-2 pr-14 font-mono text-[11px] leading-snug">
        <InPreContext.Provider value>{children}</InPreContext.Provider>
      </pre>
    </div>
  );
}

function InlineOrBlockCode({ className, children }: { className?: string; children?: ReactNode }) {
  const inPre = useContext(InPreContext);
  if (inPre) return <code className={className}>{children}</code>;
  return <code className="rounded bg-muted px-1 py-0.5 font-mono text-[11px]">{children}</code>;
}

function CopyLink({ href, children }: { href?: string; children?: ReactNode }) {
  // 被安全过滤掉的网址（比如 javascript:）是空的：只显示文字
  if (!href) return <span>{children}</span>;
  const label = textOf(children);
  return (
    <span data-section="md-link">
      <button
        type="button"
        title="点击复制链接"
        onClick={(event) => {
          event.preventDefault();
          void copyText(href, "已复制链接");
        }}
        className="break-all text-left text-primary underline decoration-dotted"
      >
        {children}
      </button>
      {label !== href && <span className="break-all text-muted-foreground"> （{href}）</span>}
    </span>
  );
}

// 组件表要定义在外面：每次渲染新对象会让 react-markdown 白白重来
const components: Components = {
  a: ({ href, children }) => <CopyLink href={href}>{children}</CopyLink>,
  img: ({ alt }) => (
    <span data-section="md-image" className="text-muted-foreground">
      [图片：{alt || "无说明"}]
    </span>
  ),
  pre: ({ children }) => <CodeBlock>{children}</CodeBlock>,
  code: ({ className, children }) => (
    <InlineOrBlockCode className={className}>{children}</InlineOrBlockCode>
  ),
  table: ({ children }) => (
    <div data-section="md-table" className="my-2 overflow-x-auto">
      <table className="w-full border-collapse text-[11px]">{children}</table>
    </div>
  ),
  th: ({ children }) => (
    <th className="border border-border/60 bg-muted/50 px-2 py-1 text-left font-medium">{children}</th>
  ),
  td: ({ children }) => <td className="border border-border/60 px-2 py-1 align-top">{children}</td>,
  h1: ({ children }) => <h1 className="mb-1.5 mt-3 text-sm font-semibold">{children}</h1>,
  h2: ({ children }) => <h2 className="mb-1.5 mt-3 text-[13px] font-semibold">{children}</h2>,
  h3: ({ children }) => <h3 className="mb-1 mt-2.5 text-xs font-semibold">{children}</h3>,
  h4: ({ children }) => <h4 className="mb-1 mt-2 text-xs font-semibold">{children}</h4>,
  h5: ({ children }) => <h5 className="mb-1 mt-2 text-xs font-medium">{children}</h5>,
  h6: ({ children }) => <h6 className="mb-1 mt-2 text-xs font-medium text-muted-foreground">{children}</h6>,
  // 段落和列表项里的单个换行照样换行（AI 常常一行一条地写），不像标准 Markdown 那样并成一行
  p: ({ children }) => <p className="my-1.5 whitespace-pre-line">{children}</p>,
  ul: ({ children }) => <ul className="my-1.5 list-disc pl-4">{children}</ul>,
  ol: ({ children }) => <ol className="my-1.5 list-decimal pl-4">{children}</ol>,
  li: ({ children }) => <li className="my-0.5 whitespace-pre-line">{children}</li>,
  blockquote: ({ children }) => (
    <blockquote className="my-1.5 border-l-2 border-border pl-2 text-muted-foreground">{children}</blockquote>
  ),
  hr: () => <hr className="my-2 border-border" />,
};

const remarkPlugins = [remarkGfm];

/**
 * 渲染一段 Markdown。`memo` 让文字没变的消息不重新解析：流式输出时只有正在写的
 * 那条每帧重画，前面已经写完的不动。
 */
export const Markdown = memo(function Markdown({ text }: { text: string }) {
  return (
    <div
      data-section="markdown"
      className="break-words text-xs leading-relaxed [&>*:first-child]:mt-0 [&>*:last-child]:mb-0"
    >
      <ReactMarkdown remarkPlugins={remarkPlugins} components={components}>
        {text}
      </ReactMarkdown>
    </div>
  );
});

/** AI 的一条文字消息：Markdown 排版，鼠标移上去右上角有"复制原文"（复制的是原始 Markdown）。 */
export function AssistantMessage({ text }: { text: string }) {
  return (
    <div data-section="assistant-message" className="group/msg relative">
      <Markdown text={text} />
      <button
        type="button"
        data-action="copy-message"
        title="复制原文"
        onClick={() => void copyText(text, "已复制")}
        className="absolute -top-1 right-0 flex items-center rounded border border-border/60 bg-background/90 p-1 text-muted-foreground opacity-0 hover:text-foreground focus-visible:opacity-100 group-hover/msg:opacity-100"
      >
        <Copy size={11} />
      </button>
    </div>
  );
}
