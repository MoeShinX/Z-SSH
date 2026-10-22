import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { isFileWindow } from "./api/fileWindow";
import { ToastProvider } from "./components/ui/toast";
import "./index.css";

const container = document.getElementById("root");
if (!container) {
  throw new Error("index.html 里没有 #root");
}

// 同一份前端：文件窗口（后端建窗口时注入了标记）只渲染 SFTP 标签，其余都是主界面。
// 两边各自按需加载，主窗口不带文件窗口的代码，文件窗口也不带主界面（AI、设置、主机页……）。
const load = isFileWindow()
  ? import("./components/files/FileWindowApp")
  : import("./App");

void load.then(({ default: Root }) => {
  createRoot(container).render(
    <StrictMode>
      <ToastProvider>
        <Root />
      </ToastProvider>
    </StrictMode>,
  );
});
