//! SFTP 右键菜单的外观和键盘导航（M3-5 第 1 节）。内容由 `state/sftpMenu.ts` 算好传进来。
//!
//! - 位置：量出菜单的**实际**尺寸再摆，右边、下边放不下就往回挪，不会超出屏幕；
//! - 键盘：上下键在可用项里移动，回车执行，右键 / 回车打开子菜单，左键 / Esc 关子菜单或整个菜单。

import { ChevronRight } from "lucide-react";
import { useLayoutEffect, useRef, useState } from "react";

import {
  nextEnabledIndex,
  placeMenu,
  placeSubmenu,
  type MenuActionId,
  type MenuNode,
  type MenuPlacement,
} from "../../state/sftpMenu";
import { cn } from "../../lib/utils";

export interface SftpContextMenuProps {
  nodes: MenuNode[];
  /** 右键点击的位置（屏幕坐标）。 */
  x: number;
  y: number;
  onSelect: (id: MenuActionId) => void;
  onClose: () => void;
}

function viewportSize() {
  return { width: window.innerWidth, height: window.innerHeight };
}

export default function SftpContextMenu({
  nodes,
  x,
  y,
  onSelect,
  onClose,
}: SftpContextMenuProps) {
  const ref = useRef<HTMLDivElement>(null);
  const [placement, setPlacement] = useState<MenuPlacement | null>(null);
  const [active, setActive] = useState(-1);
  const [openSub, setOpenSub] = useState<number | null>(null);

  // 先隐形地渲染出来量尺寸，再摆到该去的位置。
  useLayoutEffect(() => {
    const element = ref.current;
    if (!element) return;
    const rect = element.getBoundingClientRect();
    setPlacement(
      placeMenu(
        { x, y },
        { width: rect.width, height: rect.height },
        viewportSize(),
      ),
    );
    element.focus();
  }, [x, y, nodes]);

  const activate = (node: MenuNode, index: number) => {
    if (node.kind === "separator" || node.disabled) return;
    if (node.kind === "submenu") {
      setOpenSub(index);
      return;
    }
    onSelect(node.id);
  };

  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      if (openSub !== null) setOpenSub(null);
      else onClose();
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (openSub !== null) return; // 子菜单自己处理
      const next = nextEnabledIndex(
        nodes,
        active,
        event.key === "ArrowDown" ? 1 : -1,
      );
      if (next >= 0) setActive(next);
    } else if (
      event.key === "ArrowRight" ||
      event.key === "Enter" ||
      event.key === " "
    ) {
      if (openSub !== null) return;
      const node = nodes[active];
      if (!node || node.kind === "separator") return;
      event.preventDefault();
      if (node.kind === "submenu" || event.key !== "ArrowRight")
        activate(node, active);
    } else if (event.key === "ArrowLeft") {
      if (openSub !== null) {
        event.preventDefault();
        setOpenSub(null);
      }
    } else if (event.key === "Tab") {
      event.preventDefault();
    }
  };

  return (
    <>
      <div
        className="fixed inset-0 z-40"
        data-testid="menu-backdrop"
        onClick={onClose}
        onContextMenu={(event) => {
          event.preventDefault();
          onClose();
        }}
      />
      <div
        ref={ref}
        role="menu"
        tabIndex={-1}
        data-testid="sftp-context-menu"
        onKeyDown={onKeyDown}
        className="fixed z-50 min-w-48 overflow-y-auto rounded-md border border-border bg-popover p-1 text-xs shadow-lg outline-none"
        style={{
          left: placement?.left ?? x,
          top: placement?.top ?? y,
          maxHeight: placement?.maxHeight,
          visibility: placement ? "visible" : "hidden",
        }}
      >
        {nodes.map((node, index) =>
          node.kind === "separator" ? (
            <div
              key={`sep-${index}`}
              role="separator"
              className="my-1 h-px bg-border/60"
            />
          ) : (
            <MenuRow
              key={node.kind === "item" ? node.id : node.id}
              node={node}
              active={active === index}
              subOpen={openSub === index}
              onHover={() => {
                setActive(index);
                setOpenSub(
                  node.kind === "submenu" && !node.disabled ? index : null,
                );
              }}
              onActivate={() => activate(node, index)}
              onSelect={onSelect}
              onCloseSub={() => setOpenSub(null)}
            />
          ),
        )}
      </div>
    </>
  );
}

function MenuRow({
  node,
  active,
  subOpen,
  onHover,
  onActivate,
  onSelect,
  onCloseSub,
}: {
  node: Exclude<MenuNode, { kind: "separator" }>;
  active: boolean;
  subOpen: boolean;
  onHover: () => void;
  onActivate: () => void;
  onSelect: (id: MenuActionId) => void;
  onCloseSub: () => void;
}) {
  const rowRef = useRef<HTMLDivElement>(null);
  return (
    <div ref={rowRef} className="relative">
      <button
        type="button"
        role="menuitem"
        aria-disabled={node.disabled || undefined}
        aria-haspopup={node.kind === "submenu" ? "menu" : undefined}
        aria-expanded={node.kind === "submenu" ? subOpen : undefined}
        data-menu-id={node.id}
        data-active={active ? "true" : undefined}
        title={node.disabled ? node.reason : undefined}
        onMouseEnter={onHover}
        onClick={onActivate}
        className={cn(
          "flex w-full items-center gap-2 rounded-sm px-2 py-1 text-left",
          node.disabled
            ? "cursor-not-allowed text-muted-foreground/40"
            : active
              ? "bg-accent text-accent-foreground"
              : "hover:bg-accent hover:text-accent-foreground",
        )}
      >
        <span className="flex-1">{node.label}</span>
        {node.kind === "item" && node.shortcut && (
          <span className="ml-6 text-[11px] text-muted-foreground">
            {node.shortcut}
          </span>
        )}
        {node.kind === "submenu" && (
          <ChevronRight size={13} className="text-muted-foreground" />
        )}
      </button>
      {node.kind === "submenu" && subOpen && !node.disabled && (
        <Submenu
          anchor={rowRef}
          nodes={node.children}
          onSelect={onSelect}
          onClose={onCloseSub}
        />
      )}
    </div>
  );
}

function Submenu({
  anchor,
  nodes,
  onSelect,
  onClose,
}: {
  anchor: React.RefObject<HTMLDivElement | null>;
  nodes: MenuNode[];
  onSelect: (id: MenuActionId) => void;
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [placement, setPlacement] = useState<MenuPlacement | null>(null);
  const [active, setActive] = useState(() => nextEnabledIndex(nodes, -1, 1));

  useLayoutEffect(() => {
    const element = ref.current;
    const host = anchor.current;
    if (!element || !host) return;
    const rect = element.getBoundingClientRect();
    const hostRect = host.getBoundingClientRect();
    setPlacement(
      placeSubmenu(
        { left: hostRect.left, top: hostRect.top, right: hostRect.right },
        { width: rect.width, height: rect.height },
        viewportSize(),
      ),
    );
    element.focus();
  }, [anchor, nodes]);

  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      event.stopPropagation();
      const next = nextEnabledIndex(
        nodes,
        active,
        event.key === "ArrowDown" ? 1 : -1,
      );
      if (next >= 0) setActive(next);
    } else if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      event.stopPropagation();
      const node = nodes[active];
      if (node && node.kind === "item" && !node.disabled) onSelect(node.id);
    } else if (event.key === "ArrowLeft" || event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      onClose();
    }
  };

  return (
    <div
      ref={ref}
      role="menu"
      tabIndex={-1}
      data-testid="sftp-context-submenu"
      onKeyDown={onKeyDown}
      className="fixed z-50 min-w-40 overflow-y-auto rounded-md border border-border bg-popover p-1 text-xs shadow-lg outline-none"
      style={{
        left: placement?.left ?? 0,
        top: placement?.top ?? 0,
        maxHeight: placement?.maxHeight,
        visibility: placement ? "visible" : "hidden",
      }}
    >
      {nodes.map((node, index) =>
        node.kind === "item" ? (
          <button
            key={node.id}
            type="button"
            role="menuitem"
            aria-disabled={node.disabled || undefined}
            data-menu-id={node.id}
            title={node.disabled ? node.reason : undefined}
            onMouseEnter={() => setActive(index)}
            onClick={() => !node.disabled && onSelect(node.id)}
            className={cn(
              "flex w-full items-center gap-2 rounded-sm px-2 py-1 text-left",
              node.disabled
                ? "cursor-not-allowed text-muted-foreground/40"
                : active === index
                  ? "bg-accent text-accent-foreground"
                  : "hover:bg-accent hover:text-accent-foreground",
            )}
          >
            {node.label}
          </button>
        ) : node.kind === "separator" ? (
          <div
            key={`sep-${index}`}
            role="separator"
            className="my-1 h-px bg-border/60"
          />
        ) : null,
      )}
    </div>
  );
}
