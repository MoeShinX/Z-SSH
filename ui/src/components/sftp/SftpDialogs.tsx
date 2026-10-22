// Adapted from Netcatty components/sftp/SftpConflictDialog.tsx、SftpPermissionsDialog.tsx（布局与选项语义）
//! SFTP 的四个对话框：冲突、修改权限、新建/重命名、删除确认（M3-2 第 4 节）。

import { pickFolder } from "../../api/dialog";
import { useEffect, useMemo, useState } from "react";

import type { SftpConflictPrompt, SftpEditEvent } from "../../api/sftp";
import {
  formatSize,
  formatTime,
  validateName,
  type NameCheck,
} from "../../state/sftpPane";
import { Button } from "../ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "../ui/dialog";
import { Input } from "../ui/input";
import { cn } from "../../lib/utils";

// ---------------------------------------------------------------------------
// 冲突（M3-2 第 4 节）
// ---------------------------------------------------------------------------

export interface ConflictAnswer {
  action: "overwrite" | "skip" | "rename" | "resume";
  applyToAll: boolean;
  renameTo?: string;
}

/**
 * 冲突什么时候能选"续传"：`can_resume` 是后端算好的（目标比源小）。
 * 纯函数，单测直接调。
 */
export function conflictResumeEnabled(prompt: Pick<SftpConflictPrompt, "can_resume">): boolean {
  return prompt.can_resume;
}

/** 复制粘贴（远端到远端 / 本地到本地）的冲突：没有"传"这回事，也没有续传。 */
export function isCopyConflict(prompt: Pick<SftpConflictPrompt, "direction">): boolean {
  return prompt.direction === "remote_copy" || prompt.direction === "local_copy";
}

/** 默认选中的动作：复制粘贴默认「重命名」（建议名「名字 - 副本」，不会弄丢任何一份）；
 *  传输能续传先建议续传（最省流量），不然覆盖。 */
export function conflictDefaultAction(
  prompt: Pick<SftpConflictPrompt, "can_resume"> & Partial<Pick<SftpConflictPrompt, "direction">>,
): ConflictAnswer["action"] {
  if (prompt.direction && isCopyConflict({ direction: prompt.direction })) return "rename";
  return prompt.can_resume ? "resume" : "overwrite";
}

interface ConflictDialogProps {
  prompt: SftpConflictPrompt;
  onAnswer: (answer: ConflictAnswer) => void;
  /** 用户直接关掉对话框（Esc / 点遮罩）：按"跳过"处理（M3-2 第 1 节）。 */
  onDismissAsSkip: () => void;
}

export function SftpConflictDialog({ prompt, onAnswer, onDismissAsSkip }: ConflictDialogProps) {
  const copying = isCopyConflict(prompt);
  const canResume = conflictResumeEnabled(prompt);
  const [action, setAction] = useState<ConflictAnswer["action"]>(conflictDefaultAction(prompt));
  const [applyToAll, setApplyToAll] = useState(false);
  const [renameTo, setRenameTo] = useState(prompt.suggested_name);

  useEffect(() => {
    setAction(conflictDefaultAction(prompt));
    setApplyToAll(false);
    setRenameTo(prompt.suggested_name);
  }, [prompt.conflict_id, prompt]);

  const nameCheck: NameCheck = useMemo(
    () => (action === "rename" ? validateName(renameTo, true) : { ok: true }),
    [action, renameTo],
  );

  const confirm = () => {
    onAnswer({
      action,
      applyToAll,
      renameTo: action === "rename" ? renameTo : undefined,
    });
  };

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onDismissAsSkip();
      }}
    >
      <DialogContent className="sm:max-w-[560px]">
        <DialogHeader>
          <DialogTitle>目标已存在</DialogTitle>
          <DialogDescription className="break-all">
            {prompt.target}
          </DialogDescription>
        </DialogHeader>

        <div className="grid grid-cols-2 gap-3">
          <div className="rounded-md border border-border/60 bg-secondary/30 p-3 text-sm">
            <div className="mb-2 font-medium">{copying ? "源（要复制的）" : "源（要传过来的）"}</div>
            <dl className="space-y-1 text-muted-foreground">
              <div className="flex justify-between gap-2">
                <dt>大小</dt>
                <dd className="text-foreground">{formatSize(prompt.source_size)}</dd>
              </div>
              <div className="flex justify-between gap-2">
                <dt>修改时间</dt>
                <dd className="text-foreground">{formatTime(prompt.source_mtime)}</dd>
              </div>
            </dl>
          </div>
          <div className="rounded-md border border-border/60 bg-secondary/30 p-3 text-sm">
            <div className="mb-2 font-medium">目标（已经存在的）</div>
            <dl className="space-y-1 text-muted-foreground">
              <div className="flex justify-between gap-2">
                <dt>大小</dt>
                <dd className="text-foreground">{formatSize(prompt.target_size)}</dd>
              </div>
              <div className="flex justify-between gap-2">
                <dt>修改时间</dt>
                <dd className="text-foreground">{formatTime(prompt.target_mtime)}</dd>
              </div>
            </dl>
          </div>
        </div>

        <div className="grid grid-cols-2 gap-2">
          <ActionOption
            label="覆盖"
            hint="替换目标文件"
            selected={action === "overwrite"}
            onClick={() => setAction("overwrite")}
          />
          <ActionOption
            label="跳过"
            hint={copying ? "这个文件不复制" : "这个文件不传"}
            selected={action === "skip"}
            onClick={() => setAction("skip")}
          />
          <ActionOption
            label="重命名"
            hint={copying ? "换个名字再复制" : "换个名字再传"}
            selected={action === "rename"}
            onClick={() => setAction("rename")}
          />
          {!copying && (
            <ActionOption
              label="续传"
              hint={canResume ? "从目标已有的部分接着传" : "目标不比源小，不能续传"}
              selected={action === "resume"}
              disabled={!canResume}
              onClick={() => setAction("resume")}
            />
          )}
        </div>

        {action === "rename" && (
          <div className="space-y-1">
            <Input
              value={renameTo}
              onChange={(event) => setRenameTo(event.target.value)}
              placeholder="新名字"
              autoFocus
            />
            {!nameCheck.ok && (
              <p className="text-xs text-destructive">{nameCheck.reason}</p>
            )}
          </div>
        )}

        <label className="flex cursor-pointer items-center gap-2 text-sm text-muted-foreground">
          <input
            type="checkbox"
            checked={applyToAll}
            onChange={(event) => setApplyToAll(event.target.checked)}
            className="accent-primary"
          />
          对后续所有冲突都这样处理
        </label>

        <DialogFooter>
          <Button variant="outline" onClick={onDismissAsSkip}>
            取消（按跳过）
          </Button>
          <Button onClick={confirm} disabled={action === "rename" && !nameCheck.ok}>
            确定
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function ActionOption({
  label,
  hint,
  selected,
  disabled,
  onClick,
}: {
  label: string;
  hint: string;
  selected: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className={cn(
        "rounded-md border p-2.5 text-left transition-colors",
        selected
          ? "border-primary bg-primary/10"
          : "border-border/60 hover:bg-accent hover:text-accent-foreground",
        disabled && "cursor-not-allowed opacity-40 hover:bg-transparent",
      )}
    >
      <div className="text-sm font-medium">{label}</div>
      <div className="text-xs text-muted-foreground">{hint}</div>
    </button>
  );
}

// ---------------------------------------------------------------------------
// 修改权限（M3-2 第 4 节：勾选和八进制联动）
// ---------------------------------------------------------------------------

/** 勾选矩阵 → 八进制（纯函数，单测直接调）。 */
export function matrixToOctal(matrix: PermMatrix): number {
  const digit = (bits: { read: boolean; write: boolean; execute: boolean }) =>
    (bits.read ? 4 : 0) + (bits.write ? 2 : 0) + (bits.execute ? 1 : 0);
  return digit(matrix.owner) * 64 + digit(matrix.group) * 8 + digit(matrix.others);
}

/** 八进制 → 勾选矩阵。非法输入返回 null（联动框里不填非法值）。 */
export function octalToMatrix(mode: number): PermMatrix | null {
  if (!Number.isInteger(mode) || mode < 0 || mode > 0o777) return null;
  const bits = (digit: number) => ({
    read: (digit & 4) !== 0,
    write: (digit & 2) !== 0,
    execute: (digit & 1) !== 0,
  });
  return {
    owner: bits((mode >> 6) & 7),
    group: bits((mode >> 3) & 7),
    others: bits(mode & 7),
  };
}

export interface PermMatrix {
  owner: { read: boolean; write: boolean; execute: boolean };
  group: { read: boolean; write: boolean; execute: boolean };
  others: { read: boolean; write: boolean; execute: boolean };
}

/** 对话框的初始权限位：服务器给的 mode 带文件类型位（0o100644），只取低 9 位；
 *  没有值就用 0o644。 */
export function initialPermissionBits(mode: number | undefined): number {
  return mode === undefined ? 0o644 : mode & 0o777;
}

/** 解析八进制输入框（"644"/"0640"）：非法返回 null。 */
export function parseOctalInput(text: string): number | null {
  const trimmed = text.trim();
  if (!/^[0-7]{3,4}$/.test(trimmed)) return null;
  return parseInt(trimmed.length === 4 ? trimmed.slice(1) : trimmed, 8);
}

interface PermissionsDialogProps {
  /** 目标条目（远端）。 */
  name: string;
  initialMode: number | undefined;
  isDir: boolean;
  onApply: (mode: number, recursive: boolean) => void;
  onCancel: () => void;
}

export function SftpPermissionsDialog({
  name,
  initialMode,
  isDir,
  onApply,
  onCancel,
}: PermissionsDialogProps) {
  const initial = useMemo(
    () => octalToMatrix(initialPermissionBits(initialMode)) ?? octalToMatrix(0o644)!,
    [initialMode],
  );
  const [matrix, setMatrix] = useState<PermMatrix>(initial);
  const [octal, setOctal] = useState(
    initialPermissionBits(initialMode).toString(8).padStart(3, "0"),
  );
  const [recursive, setRecursive] = useState(false);

  const toggle = (who: keyof PermMatrix, what: keyof PermMatrix["owner"]) => {
    const next: PermMatrix = {
      ...matrix,
      [who]: { ...matrix[who], [what]: !matrix[who][what] },
    };
    setMatrix(next);
    setOctal(matrixToOctal(next).toString(8).padStart(3, "0"));
  };

  const applyOctal = (text: string) => {
    setOctal(text);
    const parsed = parseOctalInput(text);
    if (parsed !== null) {
      const next = octalToMatrix(parsed);
      if (next) setMatrix(next);
    }
  };

  const roles: Array<{ key: keyof PermMatrix; label: string }> = [
    { key: "owner", label: "所有者" },
    { key: "group", label: "组" },
    { key: "others", label: "其他" },
  ];

  return (
    <Dialog open onOpenChange={(open) => !open && onCancel()}>
      <DialogContent className="sm:max-w-[420px]">
        <DialogHeader>
          <DialogTitle>修改权限</DialogTitle>
          <DialogDescription className="truncate">{name}</DialogDescription>
        </DialogHeader>

        <div className="space-y-3">
          <div className="grid grid-cols-[4rem_1fr_1fr_1fr] items-center gap-2 px-1 text-xs text-muted-foreground">
            <span />
            <span className="text-center">读</span>
            <span className="text-center">写</span>
            <span className="text-center">执行</span>
          </div>
          {roles.map((role) => (
            <div
              key={role.key}
              className="grid grid-cols-[4rem_1fr_1fr_1fr] items-center gap-2 rounded-md border border-border/60 px-3 py-2"
            >
              <span className="text-sm">{role.label}</span>
              {(["read", "write", "execute"] as const).map((perm) => (
                <label key={perm} className="flex justify-center">
                  <input
                    type="checkbox"
                    checked={matrix[role.key][perm]}
                    onChange={() => toggle(role.key, perm)}
                    className="accent-primary"
                  />
                </label>
              ))}
            </div>
          ))}

          <div className="flex items-center justify-between border-t border-border/60 pt-3">
            <span className="text-sm text-muted-foreground">八进制</span>
            <Input
              value={octal}
              onChange={(event) => applyOctal(event.target.value)}
              className="h-8 w-24 font-mono"
            />
          </div>

          {isDir && (
            <label className="flex cursor-pointer items-center gap-2 text-sm text-muted-foreground">
              <input
                type="checkbox"
                checked={recursive}
                onChange={(event) => setRecursive(event.target.checked)}
                className="accent-primary"
              />
              递归应用到文件夹里面的所有内容
            </label>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={onCancel}>
            取消
          </Button>
          <Button onClick={() => onApply(matrixToOctal(matrix), recursive)}>
            应用
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// ---------------------------------------------------------------------------
// 新建 / 重命名（M3-2 第 4 节：名称校验）
// ---------------------------------------------------------------------------

interface NameDialogProps {
  title: string;
  description: string;
  initial: string;
  /** true = 远端（校验只禁 `/`）；false = 本地（还要禁 Windows 非法字符）。 */
  isRemote: boolean;
  /** 已有的名字（大小写不敏感）：重命名到它时提示。 */
  existingNames?: string[];
  onConfirm: (name: string) => void;
  onCancel: () => void;
}

export function SftpNameDialog({
  title,
  description,
  initial,
  isRemote,
  existingNames,
  onConfirm,
  onCancel,
}: NameDialogProps) {
  const [name, setName] = useState(initial);
  const check = useMemo(() => validateName(name, isRemote), [name, isRemote]);
  const taken =
    existingNames?.some(
      (existing) => existing.toLowerCase() === name.trim().toLowerCase(),
    ) ?? false;

  return (
    <Dialog open onOpenChange={(open) => !open && onCancel()}>
      <DialogContent className="sm:max-w-[380px]">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription className="break-all">{description}</DialogDescription>
        </DialogHeader>
        <Input
          value={name}
          onChange={(event) => setName(event.target.value)}
          // 自动全选：改名时直接输入就是替换
          onFocus={(event) => event.target.select()}
          autoFocus
          onKeyDown={(event) => {
            if (event.key === "Enter" && check.ok && !taken) onConfirm(name.trim());
          }}
        />
        {!check.ok && <p className="text-xs text-destructive">{check.reason}</p>}
        {check.ok && taken && (
          <p className="text-xs text-amber-500">已经有同名的条目，确定会覆盖 / 失败</p>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={onCancel}>
            取消
          </Button>
          <Button disabled={!check.ok} onClick={() => onConfirm(name.trim())}>
            确定
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// ---------------------------------------------------------------------------
// 编辑器保存：远端被改过（M3-3 第 5 节）
// ---------------------------------------------------------------------------

interface EditConflictDialogProps {
  event: SftpEditEvent;
  /** 选了覆盖 / 放弃 / 另存为（带新名字）。 */
  onResolve: (action: "overwrite" | "discard" | "save_as", name?: string) => void;
  /** 先不处理：对话框收起，这个文件的修改仍然算"还没上传"。 */
  onLater: () => void;
}

/**
 * 冲突提示正文。不用括号把大小和时间括起来：全角"）"后面紧跟"。"时，字体回退会让两个
 * 符号之间看起来多出一个空格（M3-3 截图 04），改成冒号引出，句末直接是时间。
 */
export function editConflictMessage(
  event: Pick<SftpEditEvent, "remote_path" | "remote_gone" | "remote_size" | "remote_mtime">,
): string {
  const fileName = event.remote_path.split("/").pop() ?? event.remote_path;
  const what = event.remote_gone
    ? `你打开「${fileName}」之后，它在服务器上被删除了。`
    : `你打开「${fileName}」之后，服务器上的这个文件又被别人改过：现在 ${formatSize(
        event.remote_size,
      )}，修改时间 ${formatTime(event.remote_mtime)}。`;
  return `${what}为了不盖掉别人的修改，这次保存还没有上传。`;
}

export function SftpEditConflictDialog({ event, onResolve, onLater }: EditConflictDialogProps) {
  const [saveAs, setSaveAs] = useState(event.suggested_name);
  const check = useMemo(() => validateName(saveAs, true), [saveAs]);
  return (
    <Dialog open onOpenChange={(open) => !open && onLater()}>
      <DialogContent className="sm:max-w-[520px]" data-section="edit-conflict">
        <DialogHeader>
          <DialogTitle>远端文件已经被改过</DialogTitle>
          <DialogDescription className="break-all">{event.remote_path}</DialogDescription>
        </DialogHeader>
        <p className="text-sm text-muted-foreground">{editConflictMessage(event)}</p>
        <div className="space-y-1">
          <label className="text-xs text-muted-foreground">另存为（同一目录下的新文件）</label>
          <Input
            value={saveAs}
            onChange={(event) => setSaveAs(event.target.value)}
            onFocus={(event) => event.target.select()}
          />
          {!check.ok && <p className="text-xs text-destructive">{check.reason}</p>}
        </div>
        <DialogFooter className="flex-wrap gap-2 sm:justify-between">
          <Button variant="ghost" onClick={onLater}>
            稍后处理
          </Button>
          <div className="flex flex-wrap gap-2">
            <Button variant="outline" onClick={() => onResolve("discard")}>
              放弃我的修改
            </Button>
            <Button
              variant="outline"
              disabled={!check.ok}
              onClick={() => onResolve("save_as", saveAs.trim())}
            >
              另存为
            </Button>
            <Button variant="destructive" onClick={() => onResolve("overwrite")}>
              覆盖远端
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// ---------------------------------------------------------------------------
// 下载到…（M3-3 第 4 节）：WebView 里做不了"拖出到资源管理器"，用它替代
// ---------------------------------------------------------------------------

interface DownloadToDialogProps {
  /** 要下载的条目数（标题里显示）。 */
  count: number;
  /** 输入框的初始值：本地栏当前的目录。 */
  initialDir: string;
  /** 目录存不存在由调用方查；返回错误文字就留在对话框里显示。 */
  onConfirm: (dir: string) => Promise<string | null>;
  onCancel: () => void;
}

/** 本地目录路径的基本检查（不是"有没有这个目录"，那个要问后端）。 */
export function validateLocalDir(raw: string): NameCheck {
  const value = raw.trim();
  if (!value) return { ok: false, reason: "请输入一个本地目录" };
  if (/[<>"|?*]/.test(value.replace(/^[A-Za-z]:/, ""))) {
    return { ok: false, reason: "路径里有 Windows 不允许的字符" };
  }
  return { ok: true };
}

export function SftpDownloadToDialog({
  count,
  initialDir,
  onConfirm,
  onCancel,
}: DownloadToDialogProps) {
  const [dir, setDir] = useState(initialDir);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const check = useMemo(() => validateLocalDir(dir), [dir]);

  const submit = async () => {
    if (!check.ok || busy) return;
    setBusy(true);
    const failure = await onConfirm(dir.trim());
    setBusy(false);
    if (failure) setError(failure);
  };

  return (
    <Dialog open onOpenChange={(open) => !open && onCancel()}>
      <DialogContent className="sm:max-w-[460px]">
        <DialogHeader>
          <DialogTitle>下载到…</DialogTitle>
          <DialogDescription>把选中的 {count} 项下载到这台电脑的哪个目录</DialogDescription>
        </DialogHeader>
        <div className="flex gap-2">
          <Input
            value={dir}
            onChange={(event) => {
              setDir(event.target.value);
              setError(null);
            }}
            onFocus={(event) => event.target.select()}
            autoFocus
            placeholder="例如 D:\下载"
            onKeyDown={(event) => {
              if (event.key === "Enter") void submit();
            }}
          />
          {/* 输入框留着当备用；平时点这里用系统的选择文件夹对话框 */}
          <Button
            variant="outline"
            data-action="pick-folder"
            onClick={() => {
              void pickFolder(dir.trim() || initialDir, "选择下载到的文件夹")
                .then((picked) => {
                  if (picked) {
                    setDir(picked);
                    setError(null);
                  }
                })
                .catch((failure: unknown) => setError(`打开选择对话框失败：${String(failure)}`));
            }}
          >
            选择文件夹…
          </Button>
        </div>
        {!check.ok && dir.trim() !== "" && (
          <p className="text-xs text-destructive">{check.reason}</p>
        )}
        {error && <p className="text-xs text-destructive break-all">{error}</p>}
        <DialogFooter>
          <Button variant="outline" onClick={onCancel}>
            取消
          </Button>
          <Button disabled={!check.ok || busy} onClick={() => void submit()}>
            下载
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// ---------------------------------------------------------------------------
// 删除确认（M3-2 第 4 节）
// ---------------------------------------------------------------------------

interface DeleteConfirmProps {
  /** 要删的条目数。 */
  count: number;
  /** 要删的条目名字（对话框里列出来，确认删的是哪些；太多时只列前面几个）。 */
  names?: string[];
  /** 里面有文件夹吗（提示"会删除里面的全部内容"）。 */
  hasFolder: boolean;
  isRemote: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

/** 删除确认框里最多列几个名字（再多就写"等 N 项"）。 */
export const DELETE_NAMES_SHOWN = 8;

export function SftpDeleteConfirm({
  count,
  names = [],
  hasFolder,
  isRemote,
  onConfirm,
  onCancel,
}: DeleteConfirmProps) {
  return (
    <Dialog open onOpenChange={(open) => !open && onCancel()}>
      <DialogContent className="sm:max-w-[400px]">
        <DialogHeader>
          <DialogTitle>删除{isRemote ? "远端" : "本地"}的 {count} 项？</DialogTitle>
          <DialogDescription>
            {hasFolder
              ? "选中的里有文件夹，会连同里面的全部内容一起删除。"
              : "选中的都是文件，删除后无法恢复。"}
          </DialogDescription>
        </DialogHeader>
        {names.length > 0 && (
          <ul
            data-section="delete-names"
            className="max-h-48 space-y-0.5 overflow-y-auto rounded-md border border-border/60 bg-muted/40 px-3 py-2 font-mono text-xs"
          >
            {names.slice(0, DELETE_NAMES_SHOWN).map((name, index) => (
              <li key={`${index}-${name}`} className="truncate" title={name}>
                {name}
              </li>
            ))}
            {names.length > DELETE_NAMES_SHOWN && (
              <li className="text-muted-foreground">
                …等另外 {names.length - DELETE_NAMES_SHOWN} 项
              </li>
            )}
          </ul>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={onCancel}>
            取消
          </Button>
          <Button variant="destructive" onClick={onConfirm}>
            删除
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** 递归删除的进度对话框（可以取消；结束告诉用户删了多少）。 */
export function SftpDeleteProgressDialog({
  deleted,
  current,
  onCancel,
}: {
  deleted: number;
  current: string;
  onCancel: () => void;
}) {
  return (
    <Dialog open>
      <DialogContent className="sm:max-w-[480px]">
        <DialogHeader>
          <DialogTitle>正在删除…</DialogTitle>
          <DialogDescription className="break-all">
            已删除 {deleted} 项；正在删：{current || "—"}
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="outline" onClick={onCancel}>
            取消删除
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
