// Adapted from Netcatty components/ui/toast.tsx
//! 全局提示条：右上角/右下角弹出的成功、错误、警告、信息。
//!
//! 用法：<ToastProvider> 包住应用，然后 useToastActions() 拿 showToast；
//! 组件外（比如事件回调）可以直接用导出的 toast.success(...) 这类快捷方法。
import { AlertCircle, AlertTriangle, CheckCircle, Info, X } from "lucide-react";
import React, { createContext, useCallback, useContext, useEffect, useMemo, useState } from "react";

import { cn } from "../../lib/utils";

export type ToastType = "success" | "error" | "warning" | "info";

export interface Toast {
  id: string;
  type: ToastType;
  title?: string;
  message: string;
  duration?: number;
  onClick?: () => void;
  actionLabel?: string;
}

interface ToastActionsValue {
  showToast: (toast: Omit<Toast, "id">) => void;
  dismissToast: (id: string) => void;
}

interface ToastStateValue {
  toasts: Toast[];
}

const ToastActionsContext = createContext<ToastActionsValue | null>(null);
const ToastStateContext = createContext<ToastStateValue | null>(null);

/** Actions-only subscription — does not re-render when the toast list changes. */
export const useToastActions = () => {
  const context = useContext(ToastActionsContext);
  if (!context) {
    throw new Error("useToastActions must be used within a ToastProvider");
  }
  return context;
};

// Simple hook for components that may not be inside ToastProvider
let globalShowToast: ((toast: Omit<Toast, "id">) => void) | null = null;

export interface ToastOptions {
  title?: string;
  duration?: number;
  onClick?: () => void;
  actionLabel?: string;
}

export const toast = {
  success: (message: string, titleOrOptions?: string | ToastOptions) => {
    const options = typeof titleOrOptions === "string" ? { title: titleOrOptions } : titleOrOptions;
    globalShowToast?.({ type: "success", message, duration: 3000, ...options });
  },
  error: (message: string, titleOrOptions?: string | ToastOptions) => {
    const options = typeof titleOrOptions === "string" ? { title: titleOrOptions } : titleOrOptions;
    globalShowToast?.({ type: "error", message, duration: 5000, ...options });
  },
  warning: (message: string, titleOrOptions?: string | ToastOptions) => {
    const options = typeof titleOrOptions === "string" ? { title: titleOrOptions } : titleOrOptions;
    globalShowToast?.({ type: "warning", message, duration: 4000, ...options });
  },
  info: (message: string, titleOrOptions?: string | ToastOptions) => {
    const options = typeof titleOrOptions === "string" ? { title: titleOrOptions } : titleOrOptions;
    globalShowToast?.({ type: "info", message, duration: 3000, ...options });
  },
};

const TOAST_ICONS: Record<ToastType, React.ReactNode> = {
  success: <CheckCircle className="h-4 w-4 text-emerald-500" />,
  error: <AlertCircle className="h-4 w-4 text-red-500" />,
  warning: <AlertTriangle className="h-4 w-4 text-yellow-500" />,
  info: <Info className="h-4 w-4 text-blue-500" />,
};

const TOAST_STYLES: Record<ToastType, string> = {
  success: "border-emerald-600 bg-emerald-50 dark:bg-emerald-950",
  error: "border-red-600 bg-red-50 dark:bg-red-950",
  warning: "border-yellow-600 bg-yellow-50 dark:bg-yellow-950",
  info: "border-blue-600 bg-blue-50 dark:bg-blue-950",
};

export const ToastProvider: React.FC<{ children: React.ReactNode }> = ({ children }) => {
  const [toasts, setToasts] = useState<Toast[]>([]);

  const showToast = useCallback((nextToast: Omit<Toast, "id">) => {
    const id = `toast-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const created: Toast = { ...nextToast, id };
    setToasts((prev) => [...prev, created]);

    if (nextToast.duration !== 0) {
      setTimeout(
        () => setToasts((prev) => prev.filter((item) => item.id !== id)),
        nextToast.duration || 4000,
      );
    }
  }, []);

  const dismissToast = useCallback((id: string) => {
    setToasts((prev) => prev.filter((item) => item.id !== id));
  }, []);

  const actionsValue = useMemo<ToastActionsValue>(
    () => ({ showToast, dismissToast }),
    [showToast, dismissToast],
  );
  const stateValue = useMemo<ToastStateValue>(() => ({ toasts }), [toasts]);

  useEffect(() => {
    globalShowToast = showToast;
    return () => {
      globalShowToast = null;
    };
  }, [showToast]);

  return (
    <ToastActionsContext.Provider value={actionsValue}>
      <ToastStateContext.Provider value={stateValue}>
        {children}
        <ToastContainer />
      </ToastStateContext.Provider>
    </ToastActionsContext.Provider>
  );
};

/** 右下角的一条条提示。 */
const ToastContainer: React.FC = () => {
  const { toasts } = useContext(ToastStateContext) ?? { toasts: [] as Toast[] };
  const actions = useContext(ToastActionsContext);
  const onDismiss = actions?.dismissToast;

  if (toasts.length === 0 || !onDismiss) return null;

  const handleToastClick = (toastItem: Toast) => {
    if (toastItem.onClick) {
      toastItem.onClick();
      onDismiss(toastItem.id);
    }
  };

  return (
    <div className="fixed bottom-4 right-4 z-[9999] flex max-w-sm flex-col gap-2">
      {toasts.map((toastItem) => (
        <div
          key={toastItem.id}
          className={cn(
            "flex items-start gap-3 rounded-lg border p-3 shadow-lg",
            "bg-card",
            TOAST_STYLES[toastItem.type],
            toastItem.onClick && "cursor-pointer transition-opacity hover:opacity-90",
          )}
          onClick={() => handleToastClick(toastItem)}
          role={toastItem.onClick ? "button" : undefined}
          tabIndex={toastItem.onClick ? 0 : undefined}
        >
          <div className="mt-0.5 flex-shrink-0">{TOAST_ICONS[toastItem.type]}</div>
          <div className="min-w-0 flex-1">
            {toastItem.title && (
              <div className="text-sm font-medium text-foreground">{toastItem.title}</div>
            )}
            <div className="break-words text-sm text-muted-foreground">{toastItem.message}</div>
            {toastItem.actionLabel && toastItem.onClick && (
              <div className="mt-1 text-xs font-medium text-primary">
                {toastItem.actionLabel} →
              </div>
            )}
          </div>
          <button
            onClick={(event) => {
              event.stopPropagation();
              onDismiss(toastItem.id);
            }}
            className="flex-shrink-0 rounded p-1 transition-colors hover:bg-secondary/80"
          >
            <X className="h-3.5 w-3.5 text-muted-foreground" />
          </button>
        </div>
      ))}
    </div>
  );
};

export default ToastProvider;
