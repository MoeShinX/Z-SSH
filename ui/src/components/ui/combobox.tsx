//! 可输入的下拉框（combobox）：输入框本身就是值，可以直接敲一个新值；点输入框或右边的箭头
//! 弹出已有选项，点一个就填进去。弹层用项目里的 Popover，跟随深色 / 浅色主题
//! （原来用浏览器自带的 `<datalist>`，弹出来的下拉不跟主题）。

import { Check, ChevronDown } from "lucide-react";
import { useId, useRef, useState } from "react";

import { cn } from "../../lib/utils";
import { Input } from "./input";
import { Popover, PopoverAnchor, PopoverContent } from "./popover";

interface ComboboxProps {
  value: string;
  onChange: (value: string) => void;
  /** 已有的选项。 */
  options: string[];
  placeholder?: string;
  className?: string;
  "aria-label"?: string;
}

export function Combobox({ value, onChange, options, placeholder, className, "aria-label": ariaLabel }: ComboboxProps) {
  const listId = useId();
  const rootRef = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  // 用户在打开之后敲过字才按输入的内容过滤；点开时（输入框里已经是某个选项）列出全部
  const [typed, setTyped] = useState(false);
  const [active, setActive] = useState(-1);

  const needle = value.trim().toLowerCase();
  const shown = typed && needle !== "" ? options.filter((name) => name.toLowerCase().includes(needle)) : options;
  const visible = open && shown.length > 0;

  const show = () => {
    setTyped(false);
    setActive(-1);
    setOpen(true);
  };
  const pick = (name: string) => {
    onChange(name);
    setOpen(false);
  };

  return (
    <Popover open={visible} onOpenChange={(next) => !next && setOpen(false)}>
      <PopoverAnchor asChild>
        <div ref={rootRef} className="relative">
          <Input
            role="combobox"
            aria-label={ariaLabel}
            aria-expanded={visible}
            aria-controls={visible ? listId : undefined}
            aria-autocomplete="list"
            autoComplete="off"
            value={value}
            placeholder={placeholder}
            className={cn(options.length > 0 && "pr-8", className)}
            onChange={(event) => {
              onChange(event.target.value);
              setTyped(true);
              setActive(-1);
              setOpen(true);
            }}
            onFocus={show}
            onClick={() => !open && show()}
            onKeyDown={(event) => {
              if (event.nativeEvent.isComposing) return;
              if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                event.preventDefault();
                if (!visible) {
                  show();
                  return;
                }
                const step = event.key === "ArrowDown" ? 1 : -1;
                if (shown.length === 0) return;
                // 还没选中任何一项（-1）：↓ 到第一项、↑ 到最后一项；之后首尾循环
                setActive((current) =>
                  current < 0 ? (step > 0 ? 0 : shown.length - 1) : (current + step + shown.length) % shown.length,
                );
              } else if (event.key === "Enter" && visible && active >= 0 && shown[active] !== undefined) {
                event.preventDefault();
                pick(shown[active]);
              } else if (event.key === "Escape" && visible) {
                event.preventDefault();
                event.stopPropagation();
                setOpen(false);
              } else if (event.key === "Tab") {
                setOpen(false);
              }
            }}
          />
          {options.length > 0 && (
            <button
              type="button"
              tabIndex={-1}
              title="选择已有分组"
              aria-label="展开选项"
              // 不抢输入框的焦点
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => (visible ? setOpen(false) : show())}
              className="absolute right-1 top-1/2 flex h-6 w-6 -translate-y-1/2 cursor-pointer items-center justify-center rounded text-muted-foreground hover:bg-foreground/10 hover:text-foreground"
            >
              <ChevronDown size={14} className={cn("transition-transform", visible && "rotate-180")} />
            </button>
          )}
        </div>
      </PopoverAnchor>
      <PopoverContent
        align="start"
        className="max-h-56 overflow-y-auto p-1"
        style={{ width: "var(--radix-popover-trigger-width)" }}
        // 焦点留在输入框里；点在输入框 / 箭头上不算点到外面
        onOpenAutoFocus={(event) => event.preventDefault()}
        onCloseAutoFocus={(event) => event.preventDefault()}
        onInteractOutside={(event) => {
          if (rootRef.current?.contains(event.target as Node)) event.preventDefault();
        }}
      >
        <div id={listId} role="listbox">
          {shown.map((name, index) => (
            <div
              key={name}
              role="option"
              aria-selected={name === value}
              data-active={index === active ? "true" : undefined}
              // 不让输入框失焦
              onMouseDown={(event) => event.preventDefault()}
              onMouseEnter={() => setActive(index)}
              onClick={() => pick(name)}
              className={cn(
                "flex cursor-pointer items-center gap-2 rounded-sm px-2 py-1.5 text-sm",
                index === active ? "bg-accent text-accent-foreground" : "hover:bg-accent/60",
              )}
            >
              <Check className={cn("h-3.5 w-3.5 shrink-0", name !== value && "opacity-0")} />
              <span className="truncate">{name}</span>
            </div>
          ))}
        </div>
      </PopoverContent>
    </Popover>
  );
}
