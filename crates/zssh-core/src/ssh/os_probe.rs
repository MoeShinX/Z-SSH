//! 连上之后探一次远端是什么系统，主机列表里换成对应的系统图标。
//!
//! 只读 `/etc/os-release`（没有就看 `uname -s`），结果归到一个固定的小集合里；认不出来就
//! 不给，界面继续用通用的服务器图标。

/// 在远端跑的命令：只读，不依赖 bash。
pub const OS_PROBE_COMMAND: &str = "cat /etc/os-release 2>/dev/null || uname -s 2>/dev/null";

/// 能识别出来的系统（和前端 `ui/src/assets/os/` 里的图标一一对应）。
pub const OS_IDS: &[&str] = &[
    "ubuntu",
    "debian",
    "centos",
    "rocky",
    "fedora",
    "arch",
    "alpine",
    "amazon",
    "opensuse",
    "redhat",
    "oracle",
    "kali",
    "almalinux",
    "alinux",
    "openeuler",
    "freebsd",
    "macos",
    "linux",
];

/// 一个名字（os-release 的 ID / ID_LIKE / NAME，或者 uname 的输出）→ 系统 ID。
fn normalize(value: &str) -> Option<&'static str> {
    let v = value.trim().to_ascii_lowercase();
    if v.is_empty() {
        return None;
    }
    let has = |needle: &str| v.contains(needle);
    Some(if v == "darwin" || has("macos") || has("mac os") {
        "macos"
    } else if has("freebsd") {
        "freebsd"
    } else if has("ubuntu") {
        "ubuntu"
    } else if has("debian") {
        "debian"
    } else if has("centos") {
        "centos"
    } else if has("rocky") {
        "rocky"
    } else if has("fedora") {
        "fedora"
    } else if has("arch") || has("manjaro") {
        "arch"
    } else if has("alpine") {
        "alpine"
    } else if has("amzn") || has("amazon") {
        "amazon"
    } else if has("opensuse") || has("suse") || has("sles") {
        "opensuse"
    } else if has("red hat") || has("redhat") || has("rhel") {
        "redhat"
    } else if has("almalinux") {
        "almalinux"
    } else if has("oracle") || v == "ol" {
        "oracle"
    } else if has("kali") {
        "kali"
    } else if has("openeuler") {
        "openeuler"
    } else if has("alinux") || has("aliyun") || has("alibaba cloud") {
        // 要在通用的 linux 之前：'alinux' 里也有 'linux'
        "alinux"
    } else if has("linux") {
        "linux"
    } else {
        return None;
    })
}

fn os_release_field<'a>(output: &'a str, key: &str) -> Option<&'a str> {
    output.lines().find_map(|line| {
        let rest = line.trim().strip_prefix(key)?.strip_prefix('=')?;
        Some(rest.trim().trim_matches(|c| c == '"' || c == '\''))
    })
}

/// 探测命令的输出 → 系统 ID。先看 `ID`，再看 `ID_LIKE`（衍生发行版认它的上游），
/// 再看 `NAME`；都没有就当成 `uname -s` 的输出。
pub fn classify(output: &str) -> Option<&'static str> {
    let has_os_release = output.lines().any(|line| line.trim().starts_with("ID="));
    if has_os_release {
        if let Some(id) = os_release_field(output, "ID").and_then(normalize) {
            if id != "linux" {
                return Some(id);
            }
        }
        if let Some(like) = os_release_field(output, "ID_LIKE") {
            if let Some(id) = like
                .split_whitespace()
                .find_map(|word| normalize(word).filter(|id| *id != "linux"))
            {
                return Some(id);
            }
        }
        if let Some(id) = os_release_field(output, "NAME").and_then(normalize) {
            return Some(id);
        }
        return Some("linux");
    }
    let first = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    // uname -s 只有一个词；别的东西（报错信息之类）不认
    if first.split_whitespace().count() != 1 {
        return None;
    }
    normalize(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_ids() {
        let debian = "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nNAME=\"Debian GNU/Linux\"\nID=debian\n";
        assert_eq!(classify(debian), Some("debian"));
        assert_eq!(
            classify("NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\n"),
            Some("ubuntu")
        );
        assert_eq!(
            classify("ID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\n"),
            Some("rocky")
        );
        assert_eq!(classify("ID=\"amzn\"\n"), Some("amazon"));
        assert_eq!(classify("ID=\"ol\"\n"), Some("oracle"));
        assert_eq!(
            classify("ID=alinux\nNAME=\"Alibaba Cloud Linux\"\n"),
            Some("alinux")
        );
        assert_eq!(classify("ID=opensuse-leap\n"), Some("opensuse"));
    }

    #[test]
    fn derivatives_fall_back_to_id_like_then_generic_linux() {
        assert_eq!(
            classify("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n"),
            Some("ubuntu")
        );
        assert_eq!(
            classify("ID=pop\nID_LIKE=\"ubuntu debian\"\n"),
            Some("ubuntu")
        );
        assert_eq!(classify("ID=nixos\nNAME=NixOS\n"), Some("linux"));
    }

    #[test]
    fn uname_fallback_and_garbage() {
        assert_eq!(classify("FreeBSD\n"), Some("freebsd"));
        assert_eq!(classify("Darwin\n"), Some("macos"));
        assert_eq!(classify("Linux\n"), Some("linux"));
        assert_eq!(classify(""), None);
        assert_eq!(
            classify("'cat' is not recognized as an internal or external command"),
            None
        );
        assert_eq!(classify("SunOS\n"), None);
    }

    #[test]
    fn every_result_is_a_known_id() {
        for sample in [
            "ID=debian",
            "ID=fedora",
            "ID=arch",
            "ID=alpine",
            "ID=kali",
            "ID=almalinux",
            "ID=openEuler",
            "ID=centos",
            "ID=rhel",
            "Darwin",
            "FreeBSD",
            "Linux",
        ] {
            let id = classify(sample).unwrap();
            assert!(OS_IDS.contains(&id), "{sample} → {id}");
        }
    }
}
