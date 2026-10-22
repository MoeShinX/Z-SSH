// Adapted from Netcatty components/cloud-sync/ (CloudSyncControls, CloudSyncDashboardTabs)
//! 设置页的“同步”一节：连接存储、主密码、同步、冲突、本地备份、离线备份文件。

import { useEffect } from "react";

import { useSync } from "../../../stores/sync";
import BackupsCard from "./BackupsCard";
import ConflictsCard from "./ConflictsCard";
import ConnectionCard from "./ConnectionCard";
import OfflineBackupCard from "./OfflineBackupCard";
import PasswordCard from "./PasswordCard";
import SyncCard from "./SyncCard";

export default function SyncSection() {
  const status = useSync((state) => state.status);
  const load = useSync((state) => state.load);

  useEffect(() => {
    void load().catch(() => {});
  }, [load]);

  return (
    <>
      <ConnectionCard status={status} />
      {status?.configured && <PasswordCard status={status} />}
      {status?.configured && status.ready && <SyncCard status={status} />}
      {status?.configured && <ConflictsCard status={status} />}
      <BackupsCard />
      <OfflineBackupCard />
    </>
  );
}
