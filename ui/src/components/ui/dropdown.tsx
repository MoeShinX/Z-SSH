// Adapted from Netcatty components/ui/dropdown.tsx
import * as React from "react";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { cn } from "../../lib/utils";

interface DropdownContextValue {
  open: boolean;
  setOpen: (open: boolean) => void;
  triggerRef: React.RefObject<HTMLButtonElement | null>;
}

const DropdownContext = React.createContext<DropdownContextValue | null>(null);

function useDropdown() {
  const context = React.useContext(DropdownContext);
  if (!context) {
    throw new Error("Dropdown components must be used within a Dropdown");
  }
  return context;
}

interface DropdownProps {
  children: React.ReactNode;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}

const Dropdown: React.FC<DropdownProps> = ({
  children,
  open: controlledOpen,
  onOpenChange,
}) => {
  const [internalOpen, setInternalOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);

  const open = controlledOpen !== undefined ? controlledOpen : internalOpen;
  const setOpen = useCallback(
    (value: boolean) => {
      if (controlledOpen === undefined) {
        setInternalOpen(value);
      }
      onOpenChange?.(value);
    },
    [controlledOpen, onOpenChange],
  );

  useEffect(() => {
    const closeOnPageHidden = () => {
      if (document.visibilityState === "hidden") {
        setOpen(false);
      }
    };
    document.addEventListener("visibilitychange", closeOnPageHidden);
    return () => document.removeEventListener("visibilitychange", closeOnPageHidden);
  }, [setOpen]);

  return (
    <DropdownContext.Provider value={{ open, setOpen, triggerRef }}>
      {children}
    </DropdownContext.Provider>
  );
};

interface DropdownTriggerProps {
  children: React.ReactElement;
  asChild?: boolean;
  toggleOnClick?: boolean;
}

const DropdownTrigger: React.FC<DropdownTriggerProps> = ({
  children,
  asChild,
  toggleOnClick = true,
}) => {
  const { open, setOpen, triggerRef } = useDropdown();

  const handleClick = (e: React.MouseEvent) => {
    e.stopPropagation();
    setOpen(!open);
  };

  if (asChild && React.isValidElement(children)) {
    return React.cloneElement(
      children as React.ReactElement<{
        ref?: React.Ref<HTMLButtonElement>;
        onClick?: (e: React.MouseEvent) => void;
      }>,
      {
        ref: triggerRef,
        onClick: (e: React.MouseEvent) => {
          const childProps = children.props as {
            onClick?: (e: React.MouseEvent) => void;
          };
          childProps?.onClick?.(e);
          if (toggleOnClick && !e.defaultPrevented) {
            handleClick(e);
          }
        },
      },
    );
  }

  return (
    <button ref={triggerRef} onClick={handleClick}>
      {children}
    </button>
  );
};

interface DropdownContentProps {
  children: React.ReactNode;
  className?: string;
  align?: "start" | "center" | "end";
  sideOffset?: number;
  side?: "top" | "bottom";
  /** If true, align to the trigger's parent element instead of the trigger itself */
  alignToParent?: boolean;
  onMouseEnter?: React.MouseEventHandler<HTMLDivElement>;
  onMouseLeave?: React.MouseEventHandler<HTMLDivElement>;
}

const DropdownContent: React.FC<DropdownContentProps> = ({
  children,
  className,
  align = "start",
  sideOffset = 4,
  side = "bottom",
  alignToParent = false,
  onMouseEnter,
  onMouseLeave,
}) => {
  const { open, setOpen, triggerRef } = useDropdown();
  const contentRef = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<{
    top: number;
    left: number;
    maxHeight?: number;
  } | null>(null);

  // Calculate position function
  const calculatePosition = useCallback(() => {
    if (!triggerRef.current) return null;

    // Use parent element if alignToParent is true
    const anchorEl = alignToParent
      ? triggerRef.current.parentElement
      : triggerRef.current;
    if (!anchorEl) return null;

    const rect = anchorEl.getBoundingClientRect();
    const triggerRect = triggerRef.current.getBoundingClientRect();
    const contentEl = contentRef.current;

    const contentHeight = contentEl?.offsetHeight ?? 0;
    const contentWidth = contentEl?.offsetWidth ?? 0;
    const viewportMargin = 8;
    const viewportWidth = window.innerWidth;
    const viewportHeight = window.innerHeight;
    const spaceBelow = Math.max(
      0,
      viewportHeight - triggerRect.bottom - sideOffset - viewportMargin,
    );
    const spaceAbove = Math.max(
      0,
      triggerRect.top - sideOffset - viewportMargin,
    );

    // Prefer requested side when it fits; otherwise pick the side with more room.
    // Never bounce oversized menus off-screen by flipping twice.
    let placeBelow: boolean;
    if (side === "bottom") {
      if (contentHeight <= spaceBelow || spaceBelow >= spaceAbove) {
        placeBelow = true;
      } else {
        placeBelow = false;
      }
    } else if (contentHeight <= spaceAbove || spaceAbove > spaceBelow) {
      placeBelow = false;
    } else {
      placeBelow = true;
    }

    const available = placeBelow ? spaceBelow : spaceAbove;
    const maxHeight =
      contentHeight > available && available > 0
        ? available
        : undefined;
    const effectiveHeight = maxHeight ?? contentHeight;

    let top: number;
    if (placeBelow) {
      top = triggerRect.bottom + sideOffset;
    } else {
      top = triggerRect.top - effectiveHeight - sideOffset;
      if (top < viewportMargin) {
        top = viewportMargin;
      }
    }

    // Use anchor element (parent or trigger) for horizontal positioning
    let left: number;
    if (align === "start") {
      left = rect.left;
    } else if (align === "end") {
      left = contentEl ? rect.right - contentWidth : rect.right;
    } else {
      left = rect.left + rect.width / 2;
      if (contentEl) {
        left -= contentWidth / 2;
      }
    }

    if (contentEl) {
      if (left + contentWidth > viewportWidth - viewportMargin) {
        left = viewportWidth - contentWidth - viewportMargin;
      }
      if (left < viewportMargin) {
        left = viewportMargin;
      }
    }

    return { top, left, maxHeight };
  }, [align, sideOffset, side, alignToParent, triggerRef]);

  // Calculate position synchronously after DOM updates
  useLayoutEffect(() => {
    if (open) {
      // Reset position first to hide content while calculating
      setPosition(null);

      // Use double requestAnimationFrame to ensure content is fully rendered
      // First frame: content is added to DOM
      // Second frame: layout is calculated, offsetWidth is available
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          const pos = calculatePosition();
          if (pos) setPosition(pos);
        });
      });
    } else {
      setPosition(null);
    }
  }, [open, calculatePosition]);

  // Close on click outside
  useEffect(() => {
    if (!open) return;

    const handleClickOutside = (e: MouseEvent) => {
      if (
        contentRef.current &&
        !contentRef.current.contains(e.target as Node) &&
        triggerRef.current &&
        !triggerRef.current.contains(e.target as Node)
      ) {
        setOpen(false);
      }
    };

    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setOpen(false);
      }
    };

    // Use setTimeout to avoid closing immediately on the same click that opened it
    const timeoutId = setTimeout(() => {
      document.addEventListener("mousedown", handleClickOutside);
      document.addEventListener("keydown", handleEscape);
    }, 0);

    return () => {
      clearTimeout(timeoutId);
      document.removeEventListener("mousedown", handleClickOutside);
      document.removeEventListener("keydown", handleEscape);
    };
  }, [open, setOpen, triggerRef]);

  if (!open) return null;

  return createPortal(
    <div
      ref={contentRef}
      className={cn(
        "fixed z-[999999] rounded-md border border-border/60 bg-popover p-1 text-popover-foreground shadow-md",
        className,
      )}
      onMouseEnter={onMouseEnter}
      onMouseLeave={onMouseLeave}
      style={{
        top: position?.top ?? -9999,
        left: position?.left ?? -9999,
        maxHeight: position?.maxHeight,
        overflowY: position?.maxHeight ? "auto" : undefined,
        visibility: position ? "visible" : "hidden",
      }}
    >
      {children}
    </div>,
    document.body,
  );
};

export { Dropdown, DropdownContent, DropdownTrigger };
