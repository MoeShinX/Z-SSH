// 主机的系统图标：探测到系统就显示发行版标志（品牌色底 + 白色标志），没探测到用 Z-SSH 图标。
// 标志和配色移植自 Netcatty（components/DistroAvatar.tsx、public/distro/），见 NOTICE。

import zsshIcon from "../../../../src-tauri/icons/zssh-z-neon/icon.ico?url";
import { useHostOsId } from "../../stores/hostOs";

const LOGOS = import.meta.glob<string>("../../assets/os/*.svg", {
  eager: true,
  query: "?url",
  import: "default",
});

function logoFor(os: string): string | undefined {
  return LOGOS[`../../assets/os/${os}.svg`];
}

export const OS_COLORS: Record<string, string> = {
  ubuntu: "#E95420",
  debian: "#A81D33",
  centos: "#9C27B0",
  rocky: "#0B9B69",
  fedora: "#3C6EB4",
  arch: "#1793D1",
  alpine: "#0D597F",
  amazon: "#FF9900",
  opensuse: "#73BA25",
  redhat: "#EE0000",
  oracle: "#C74634",
  kali: "#0F6DB3",
  almalinux: "#173B66",
  alinux: "#FF6A00",
  openeuler: "#002FA7",
  freebsd: "#AB2B28",
  macos: "#333333",
  linux: "#333333",
};

const OS_NAMES: Record<string, string> = {
  ubuntu: "Ubuntu",
  debian: "Debian",
  centos: "CentOS",
  rocky: "Rocky Linux",
  fedora: "Fedora",
  arch: "Arch Linux",
  alpine: "Alpine",
  amazon: "Amazon Linux",
  opensuse: "openSUSE",
  redhat: "Red Hat",
  oracle: "Oracle Linux",
  kali: "Kali Linux",
  almalinux: "AlmaLinux",
  alinux: "Alibaba Cloud Linux",
  openeuler: "openEuler",
  freebsd: "FreeBSD",
  macos: "macOS",
  linux: "Linux",
};

const SIZES = {
  /** 左侧主机树一行 */
  tree: { box: 16, logo: 11, radius: 4 },
  /** 主机卡片 */
  card: { box: 34, logo: 18, radius: 6 },
} as const;

export default function OsIcon({ hostId, size }: { hostId: number; size: keyof typeof SIZES }) {
  const os = useHostOsId(hostId);
  const logo = os ? logoFor(os) : undefined;
  const dims = SIZES[size];
  if (!os || !logo) {
    return (
      <img
        src={zsshIcon}
        alt=""
        draggable={false}
        width={dims.box}
        height={dims.box}
        className="shrink-0 object-contain"
      />
    );
  }
  return (
    <div
      data-os={os}
      title={OS_NAMES[os] ?? os}
      className="flex shrink-0 items-center justify-center"
      style={{
        width: dims.box,
        height: dims.box,
        borderRadius: dims.radius,
        backgroundColor: OS_COLORS[os] ?? "#333333",
      }}
    >
      <img
        src={logo}
        alt={OS_NAMES[os] ?? os}
        draggable={false}
        style={{ width: dims.logo, height: dims.logo, filter: "brightness(0) invert(1)" }}
      />
    </div>
  );
}
