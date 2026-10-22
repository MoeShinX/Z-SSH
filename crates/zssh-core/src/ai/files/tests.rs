//! 文件工具的单测：路径危险规则、差异、分段读、备份与原子写（内存文件系统，可注入失败）。

use super::testfs::MemFs;
use super::*;

// ---------- 路径规范与危险规则 ----------

#[test]
fn resolve_path_handles_home_relative_and_dots() {
    let home = Some("/home/u");
    assert_eq!(resolve_path("~", home).as_deref(), Some("/home/u"));
    assert_eq!(resolve_path("~/a/b", home).as_deref(), Some("/home/u/a/b"));
    assert_eq!(
        resolve_path("a/./b//c", home).as_deref(),
        Some("/home/u/a/b/c")
    );
    assert_eq!(resolve_path("../x", home).as_deref(), Some("/home/x"));
    assert_eq!(
        resolve_path("/a/../../etc/./passwd", home).as_deref(),
        Some("/etc/passwd")
    );
    assert_eq!(resolve_path("/", home).as_deref(), Some("/"));
    assert_eq!(resolve_path("rel", None), None);
    assert_eq!(resolve_path("/abs", None).as_deref(), Some("/abs"));
}

#[test]
fn validate_path_rejects_empty_and_nul() {
    assert!(validate_path("").is_err());
    assert!(validate_path("   ").is_err());
    assert!(validate_path("/a\0b").is_err());
    assert!(validate_path("/a/b").is_ok());
}

#[test]
fn system_paths_are_dangerous() {
    for path in [
        "/etc/hosts",
        "/etc",
        "/etc/nginx/nginx.conf",
        "/boot/grub/grub.cfg",
        "/usr/local/bin/tool",
        "/bin/sh",
        "/sbin/init",
        "/lib/x.so",
        "/lib64/ld.so",
        "/lib32/x",
        "/libx32/y",
        "/dev/sda",
        "/proc/sys/kernel/x",
        "/sys/class/x",
        "/var/spool/cron/crontabs/root",
        "/etc/sudoers",
        "/etc/sudoers.d/90-x",
        "/home/u/.ssh/authorized_keys",
        "/root/.ssh/authorized_keys",
        "/home/u/.ssh/authorized_keys2",
        "/opt/app/sudoers.bak",
        "/opt/app/crontab",
        "/opt/app/my-crontab.txt",
    ] {
        let verdict = assess_absolute_path(path);
        assert!(verdict.dangerous, "{path} 应该标红");
        assert!(!verdict.reasons.is_empty(), "{path} 要写明原因");
    }
}

#[test]
fn ordinary_paths_are_safe() {
    for path in [
        "/home/u/notes.txt",
        "/tmp/zssh-x/a.txt",
        "/opt/app/config.yml",
        "/var/www/html/index.html",
        "/srv/data/x",
        "/etcetera/file",
        "/usrlocal/x",
        "/binary/x",
        "/home/u/.ssh/config",
        "/home/u/.ssh/known_hosts",
        "/home/u/authorized_keys",
        "/home/u/library/x",
        "/",
    ] {
        let verdict = assess_absolute_path(path);
        assert!(!verdict.dangerous, "{path} 不该标红：{:?}", verdict.reasons);
    }
}

#[test]
fn top_level_lib_star_is_dangerous_but_lookalikes_inside_home_are_not() {
    assert!(assess_absolute_path("/lib").dangerous);
    assert!(assess_absolute_path("/libexec/x").dangerous);
    assert!(!assess_absolute_path("/home/u/lib/x").dangerous);
}

#[test]
fn relative_and_tilde_paths_are_resolved_before_checking() {
    let home = Some("/home/u");
    assert!(
        assess_path("../../etc/passwd", home).dangerous,
        "相对路径跳出家目录"
    );
    assert!(assess_path("~/.ssh/authorized_keys", home).dangerous);
    assert!(assess_path(".ssh/authorized_keys", home).dangerous);
    assert!(
        assess_path("/tmp/../etc/hosts", home).dangerous,
        "..折叠之后落在 /etc"
    );
    assert!(!assess_path("notes/a.txt", home).dangerous);
    // 没有家目录又有 ..：保守标红。
    assert!(assess_path("../x", None).dangerous);
    assert!(!assess_path("x/y", None).dangerous);
}

#[test]
fn merge_verdicts_keeps_every_reason_once() {
    let a = assess_absolute_path("/etc/sudoers");
    let b = assess_absolute_path("/etc/hosts");
    let merged = merge_verdicts(a.clone(), b);
    assert!(merged.dangerous);
    assert!(merged.reasons.len() >= a.reasons.len());
    let again = merge_verdicts(merged.clone(), merged.clone());
    assert_eq!(again.reasons, merged.reasons);
}

// ---------- 差异 ----------

#[test]
fn diff_marks_changed_lines_with_context() {
    let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
    let new = "a\nb\nc\nD\ne\nf\ng\nh\n";
    let diff = make_diff(Some(old), new);
    assert!(!diff.is_new);
    assert_eq!((diff.added, diff.removed), (1, 1));
    assert_eq!(diff.omitted_lines, 0);
    assert_eq!(diff.lines[0].kind, "hunk");
    assert!(diff.lines.iter().any(|l| l.kind == "del" && l.text == "d"));
    assert!(diff.lines.iter().any(|l| l.kind == "add" && l.text == "D"));
    // 上下文各 3 行：a b c … e f g
    let ctx: Vec<&str> = diff
        .lines
        .iter()
        .filter(|l| l.kind == "ctx")
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(ctx, vec!["a", "b", "c", "e", "f", "g"]);
}

#[test]
fn diff_of_identical_content_is_empty() {
    let diff = make_diff(Some("same\n"), "same\n");
    assert!(diff.lines.is_empty());
    assert_eq!((diff.added, diff.removed), (0, 0));
}

#[test]
fn diff_for_a_new_file_is_the_whole_text() {
    let diff = make_diff(None, "x\ny\nz\n");
    assert!(diff.is_new);
    assert_eq!(diff.added, 3);
    assert!(diff.lines.iter().all(|l| l.kind == "add"));
    assert_eq!(diff.lines.len(), 3);
}

#[test]
fn very_long_diff_is_truncated_and_says_how_much() {
    let new: String = (0..5_000).map(|i| format!("line {i}\n")).collect();
    let diff = make_diff(None, &new);
    assert_eq!(diff.added, 5_000);
    assert_eq!(diff.lines.len(), DIFF_MAX_LINES);
    assert_eq!(diff.omitted_lines, 5_000 - DIFF_MAX_LINES);

    let old: String = (0..5_000).map(|i| format!("old {i}\n")).collect();
    let both = make_diff(Some(&old), &new);
    assert_eq!(both.lines.len(), DIFF_MAX_LINES);
    assert!(both.omitted_lines > 0);
    assert_eq!(both.added, 5_000);
    assert_eq!(both.removed, 5_000);
}

#[test]
fn diff_shows_a_missing_trailing_newline() {
    let diff = make_diff(Some("a\nb"), "a\nb\n");
    assert!(diff.lines.iter().any(|l| l.text.contains("无换行符")));
}

#[test]
fn crlf_endings_do_not_leak_into_diff_text() {
    let diff = make_diff(None, "a\r\nb\r\n");
    assert_eq!(diff.lines[0].text, "a");
}

// ---------- 分段读 ----------

#[test]
fn plain_text_chunk_reports_next_offset_unless_eof() {
    match slice_text_chunk(b"hello", 0, false) {
        TextChunk::Text {
            content,
            start,
            next_offset,
        } => {
            assert_eq!(
                (content.as_str(), start, next_offset),
                ("hello", 0, Some(5))
            );
        }
        other => panic!("{other:?}"),
    }
    match slice_text_chunk(b"hello", 0, true) {
        TextChunk::Text { next_offset, .. } => assert_eq!(next_offset, None),
        other => panic!("{other:?}"),
    }
}

#[test]
fn chunk_ending_inside_a_character_leaves_the_tail_for_next_time() {
    let text = "ab中c"; // 中 = 3 字节
    let bytes = text.as_bytes();
    let cut = &bytes[..4]; // "ab" + 中的第 1 个字节
    match slice_text_chunk(cut, 0, false) {
        TextChunk::Text {
            content,
            next_offset,
            ..
        } => {
            assert_eq!(content, "ab");
            assert_eq!(next_offset, Some(2));
        }
        other => panic!("{other:?}"),
    }
    // 在文件尾却不完整：是坏文件，当二进制。
    assert_eq!(slice_text_chunk(cut, 0, true), TextChunk::Binary);
}

#[test]
fn chunk_starting_inside_a_character_skips_to_the_next_boundary() {
    let bytes = "中文x".as_bytes(); // 中=0..3 文=3..6 x=6
    match slice_text_chunk(&bytes[1..], 1, true) {
        TextChunk::Text { content, start, .. } => {
            assert_eq!(content, "文x");
            assert_eq!(start, 3);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn nul_bytes_and_invalid_utf8_are_binary() {
    assert_eq!(slice_text_chunk(b"ab\0cd", 0, true), TextChunk::Binary);
    assert_eq!(
        slice_text_chunk(&[0x89, b'P', b'N', b'G'], 0, true),
        TextChunk::Binary
    );
    assert_eq!(
        slice_text_chunk(&[b'a', 0xff, b'b'], 0, false),
        TextChunk::Binary
    );
}

#[tokio::test]
async fn read_file_in_segments_reassembles_the_whole_file() {
    let fs = MemFs::new();
    let original: String = (0..3000).map(|i| format!("第{i}行 data\n")).collect();
    fs.put("/home/u/big.txt", original.as_bytes(), 0o644);

    let mut got = String::new();
    let mut offset = 0u64;
    let mut calls = 0;
    loop {
        calls += 1;
        let result = tool_read_file(&fs, "big.txt", Some(offset), Some(1000))
            .await
            .expect("读");
        assert!(!result.binary);
        assert!(result.returned_bytes <= 1000, "单次不超过 max_bytes");
        assert_eq!(result.offset, offset, "分段读的起点都落在字符边界上");
        got.push_str(&result.content);
        match result.next_offset {
            Some(next) => {
                assert!(next > offset, "每一段都要有进展");
                offset = next;
            }
            None => break,
        }
        assert!(calls < 1000);
    }
    assert_eq!(got, original);
    assert!(calls > 1);
}

#[tokio::test]
async fn read_file_caps_a_single_call_at_64k() {
    let fs = MemFs::new();
    fs.put("/home/u/b.txt", &vec![b'x'; 200_000], 0o644);
    let result = tool_read_file(&fs, "/home/u/b.txt", None, Some(10_000_000))
        .await
        .unwrap();
    assert_eq!(result.returned_bytes, READ_MAX_BYTES);
    assert_eq!(result.next_offset, Some(READ_MAX_BYTES as u64));
    assert_eq!(result.size, 200_000);
}

#[tokio::test]
async fn read_file_says_binary_and_returns_no_content() {
    let fs = MemFs::new();
    fs.put("/home/u/a.bin", &[0x7f, b'E', b'L', b'F', 0, 1, 2], 0o755);
    let result = tool_read_file(&fs, "a.bin", None, None).await.unwrap();
    assert!(result.binary);
    assert!(result.content.is_empty());
}

#[tokio::test]
async fn read_file_errors_for_missing_and_directories_and_handles_end() {
    let fs = MemFs::new();
    fs.put("/home/u/a.txt", b"abc", 0o644);
    assert!(tool_read_file(&fs, "nope", None, None)
        .await
        .unwrap_err()
        .contains("不存在"));
    assert!(tool_read_file(&fs, "/home/u", None, None)
        .await
        .unwrap_err()
        .contains("list_dir"));
    let past = tool_read_file(&fs, "a.txt", Some(99), None).await.unwrap();
    assert!(past.content.is_empty());
    assert_eq!(past.next_offset, None);
    fs.put("/home/u/e.txt", b"", 0o644);
    let empty = tool_read_file(&fs, "e.txt", None, None).await.unwrap();
    assert!(!empty.binary && empty.content.is_empty());
}

// ---------- list_dir / stat ----------

#[tokio::test]
async fn list_dir_sorts_and_truncates_at_the_limit() {
    let fs = MemFs::new();
    fs.put_dir("/data");
    for i in 0..(LIST_DIR_LIMIT + 50) {
        fs.put(&format!("/data/f{i:05}"), b"x", 0o644);
    }
    let (result, path) = tool_list_dir(&fs, "/data").await.unwrap();
    assert_eq!(path, "/data");
    assert_eq!(result.entries.len(), LIST_DIR_LIMIT);
    assert_eq!(result.total, LIST_DIR_LIMIT + 50);
    assert!(result.truncated);
    assert_eq!(result.entries[0].name, "f00000");
    assert_eq!(result.entries[0].kind, "file");
    assert_eq!(result.entries[0].mode, "0644");
    assert!(result.entries[0]
        .modified
        .as_deref()
        .unwrap()
        .contains("20"));

    let (small, _) = tool_list_dir(&fs, "~").await.unwrap();
    assert!(!small.truncated);
    assert!(tool_list_dir(&fs, "/data/f00001")
        .await
        .unwrap_err()
        .contains("不是目录"));
    assert!(tool_list_dir(&fs, "/nope")
        .await
        .unwrap_err()
        .contains("不存在"));
}

#[tokio::test]
async fn stat_reports_existence_type_size_and_mode() {
    let fs = MemFs::new();
    fs.put("/home/u/a.txt", b"12345", 0o600);
    let (hit, path) = tool_stat(&fs, "a.txt").await.unwrap();
    assert_eq!(path, "/home/u/a.txt");
    assert!(hit.exists);
    assert_eq!(hit.kind.as_deref(), Some("file"));
    assert_eq!(hit.size, Some(5));
    assert_eq!(hit.mode.as_deref(), Some("0600"));
    let (miss, _) = tool_stat(&fs, "nope.txt").await.unwrap();
    assert!(!miss.exists);
    assert!(miss.kind.is_none());
    let (dir, _) = tool_stat(&fs, "~").await.unwrap();
    assert_eq!(dir.kind.as_deref(), Some("dir"));
}

// ---------- 备份名与时间 ----------

#[test]
fn timestamps_format_in_utc() {
    assert_eq!(compact_timestamp(0), "19700101-000000");
    assert_eq!(compact_timestamp(1_000_000_000), "20010909-014640");
    assert_eq!(display_timestamp(1_000_000_000), "2001-09-09 01:46:40");
    assert_eq!(compact_timestamp(951_782_400), "20000229-000000", "闰日");
    assert_eq!(compact_timestamp(-1), "19691231-235959");
}

#[test]
fn backup_and_temp_names_stay_in_the_same_directory() {
    assert_eq!(
        backup_name("/etc/app/conf.yml", 1_000_000_000, 0),
        "/etc/app/conf.yml.zssh-bak-20010909-014640"
    );
    assert_eq!(
        backup_name("/etc/app/conf.yml", 1_000_000_000, 1),
        "/etc/app/conf.yml.zssh-bak-20010909-014640-2"
    );
    assert_eq!(backup_name("/x", 0, 0), "/x.zssh-bak-19700101-000000");
    assert_eq!(temp_name("/a/b.txt", "abc123"), "/a/.b.txt.zssh-tmp-abc123");
    assert_eq!(temp_name("/b.txt", "abc123"), "/.b.txt.zssh-tmp-abc123");
}

// ---------- 写文件：准备 ----------

#[tokio::test]
async fn prepare_write_for_a_new_file_shows_the_whole_content() {
    let fs = MemFs::new();
    let plan = prepare_write(&fs, "new.txt", "hi\nthere\n", false)
        .await
        .unwrap();
    assert!(plan.is_new());
    assert_eq!(plan.target, "/home/u/new.txt");
    assert!(plan.diff.is_new);
    assert_eq!(plan.diff.added, 2);
    assert!(!plan.danger.dangerous);
}

#[tokio::test]
async fn prepare_write_flags_system_paths_but_does_not_block() {
    let fs = MemFs::new();
    fs.put_dir("/etc");
    fs.put("/etc/hosts", b"127.0.0.1 localhost\n", 0o644);
    let plan = prepare_write(
        &fs,
        "/etc/hosts",
        "127.0.0.1 localhost\n10.0.0.1 x\n",
        false,
    )
    .await
    .unwrap();
    assert!(plan.danger.dangerous);
    assert!(plan.danger.reasons.iter().any(|r| r.contains("/etc")));
    assert_eq!(plan.diff.added, 1);
    assert_eq!(plan.old_mode, Some(0o644));
}

#[tokio::test]
async fn prepare_write_rejects_bad_targets() {
    let fs = MemFs::new();
    fs.put("/home/u/a.txt", b"x", 0o644);
    fs.put("/home/u/b.bin", &[0, 1, 2], 0o644);
    assert!(prepare_write(&fs, "/home/u", "x", false)
        .await
        .unwrap_err()
        .contains("目录"));
    assert!(prepare_write(&fs, "a.txt", "x", true)
        .await
        .unwrap_err()
        .contains("create_only"));
    assert!(prepare_write(&fs, "b.bin", "x", false)
        .await
        .unwrap_err()
        .contains("二进制"));
    assert!(prepare_write(&fs, "nodir/x.txt", "x", false)
        .await
        .unwrap_err()
        .contains("上级目录不存在"));
    assert!(prepare_write(&fs, "", "x", false).await.is_err());
    let huge = "x".repeat(WRITE_MAX_BYTES as usize + 1);
    assert!(prepare_write(&fs, "big.txt", &huge, false)
        .await
        .unwrap_err()
        .contains("上限"));
    // 父级是文件，不是目录
    assert!(prepare_write(&fs, "a.txt/child", "x", false)
        .await
        .unwrap_err()
        .contains("不是目录"));
}

#[tokio::test]
async fn prepare_write_follows_symlinks_and_checks_the_real_path_too() {
    let fs = MemFs::new();
    fs.put_dir("/etc");
    fs.put("/etc/passwd", b"root\n", 0o644);
    fs.links
        .lock()
        .unwrap()
        .insert("/home/u/pw".to_owned(), "/etc/passwd".to_owned());
    let plan = prepare_write(&fs, "pw", "root\nx\n", false).await.unwrap();
    assert_eq!(plan.target, "/etc/passwd", "写链接指向的真实文件");
    assert!(
        plan.danger.dangerous,
        "链接本身在家目录，但真实位置是系统路径"
    );
}

// ---------- 写文件：执行 ----------

#[tokio::test]
async fn overwrite_makes_a_backup_with_the_old_content_then_replaces_via_rename() {
    let fs = MemFs::new();
    fs.put("/home/u/conf.txt", b"old\n", 0o640);
    let plan = prepare_write(&fs, "conf.txt", "new\n", false)
        .await
        .unwrap();
    let result = execute_write(&fs, &plan, false, 1_000_000_000, "r4nd", 5)
        .await
        .unwrap();

    assert_eq!(fs.get("/home/u/conf.txt").unwrap(), b"new\n");
    assert_eq!(
        fs.mode_of("/home/u/conf.txt"),
        Some(0o640),
        "权限沿用原文件"
    );
    let backup = result.backup_path.clone().expect("覆盖要有备份");
    assert_eq!(backup, "/home/u/conf.txt.zssh-bak-20010909-014640");
    assert_eq!(fs.get(&backup).unwrap(), b"old\n");
    assert!(!result.created);
    assert_eq!(result.bytes_written, 4);
    // 目录里没有遗留的临时文件。
    assert!(fs
        .names_in("/home/u")
        .iter()
        .all(|n| !n.contains("zssh-tmp")));
    // 新内容只通过"写临时文件 + 改名"到位，从没直接写过目标。
    let log = fs.log();
    assert!(
        !log.contains(&"write /home/u/conf.txt".to_owned()),
        "{log:?}"
    );
    assert!(log.contains(&"rename /home/u/.conf.txt.zssh-tmp-r4nd /home/u/conf.txt".to_owned()));
    // 顺序：先备份，后写临时文件，最后改名。
    let pos = |needle: &str| log.iter().position(|l| l.contains(needle)).unwrap();
    assert!(pos("zssh-bak") < pos("zssh-tmp") && pos("zssh-tmp") < pos("rename"));
}

#[tokio::test]
async fn creating_a_file_makes_no_backup() {
    let fs = MemFs::new();
    let plan = prepare_write(&fs, "fresh.txt", "x\n", true).await.unwrap();
    let result = execute_write(&fs, &plan, true, 0, "aa", 5).await.unwrap();
    assert!(result.created);
    assert!(result.backup_path.is_none());
    assert_eq!(fs.get("/home/u/fresh.txt").unwrap(), b"x\n");
    assert!(fs
        .names_in("/home/u")
        .iter()
        .all(|n| !n.contains("zssh-bak") && !n.contains("zssh-tmp")));
}

#[tokio::test]
async fn backup_failure_means_nothing_is_written() {
    let fs = MemFs::new();
    fs.put("/home/u/conf.txt", b"old\n", 0o644);
    let plan = prepare_write(&fs, "conf.txt", "new\n", false)
        .await
        .unwrap();
    *fs.fail_write_containing.lock().unwrap() = Some("zssh-bak".to_owned());
    let err = execute_write(&fs, &plan, false, 5, "r", 5)
        .await
        .unwrap_err();
    assert!(err.contains("备份失败"), "{err}");
    assert_eq!(fs.get("/home/u/conf.txt").unwrap(), b"old\n", "原文件原样");
    let log = fs.log();
    assert!(
        !log.iter()
            .any(|l| l.contains("zssh-tmp") || l.starts_with("rename")),
        "备份失败后不能再碰临时文件和改名：{log:?}"
    );
}

#[tokio::test]
async fn half_written_backup_counts_as_failure_and_is_removed() {
    let fs = MemFs::new();
    fs.put("/home/u/conf.txt", b"old content\n", 0o644);
    let plan = prepare_write(&fs, "conf.txt", "new\n", false)
        .await
        .unwrap();
    *fs.half_write_containing.lock().unwrap() = Some("zssh-bak".to_owned());
    let err = execute_write(&fs, &plan, false, 5, "r", 5)
        .await
        .unwrap_err();
    assert!(err.contains("备份失败"), "{err}");
    assert_eq!(fs.get("/home/u/conf.txt").unwrap(), b"old content\n");
    assert!(
        fs.names_in("/home/u")
            .iter()
            .all(|n| !n.contains("zssh-bak")),
        "半截备份要清掉"
    );
}

#[tokio::test]
async fn a_failed_temp_write_leaves_the_original_untouched_and_cleans_up() {
    let fs = MemFs::new();
    fs.put("/home/u/conf.txt", b"old\n", 0o644);
    let plan = prepare_write(&fs, "conf.txt", "new content that is longer\n", false)
        .await
        .unwrap();
    *fs.half_write_containing.lock().unwrap() = Some("zssh-tmp".to_owned());
    let err = execute_write(&fs, &plan, false, 5, "r", 5)
        .await
        .unwrap_err();
    assert!(err.contains("原文件没有改动"), "{err}");
    assert_eq!(
        fs.get("/home/u/conf.txt").unwrap(),
        b"old\n",
        "写一半断了，原文件不能变"
    );
    assert!(
        fs.names_in("/home/u")
            .iter()
            .all(|n| !n.contains("zssh-tmp")),
        "半截临时文件要清掉"
    );
}

#[tokio::test]
async fn a_failed_rename_cleans_the_temp_file_and_keeps_the_original() {
    let fs = MemFs::new();
    fs.put("/home/u/conf.txt", b"old\n", 0o644);
    let plan = prepare_write(&fs, "conf.txt", "new\n", false)
        .await
        .unwrap();
    *fs.fail_rename.lock().unwrap() = true;
    let err = execute_write(&fs, &plan, false, 5, "r", 5)
        .await
        .unwrap_err();
    assert!(err.contains("提交前失败，原文件没有改动"), "{err}");
    assert_eq!(fs.get("/home/u/conf.txt").unwrap(), b"old\n");
    assert!(fs
        .names_in("/home/u")
        .iter()
        .all(|n| !n.contains("zssh-tmp")));
}

#[tokio::test]
async fn changes_between_approval_and_write_cancel_the_write() {
    let fs = MemFs::new();
    fs.put("/home/u/conf.txt", b"old\n", 0o644);
    let plan = prepare_write(&fs, "conf.txt", "new\n", false)
        .await
        .unwrap();
    // 用户在确认卡片前面看了很久，期间别人改了文件。
    fs.put("/home/u/conf.txt", b"someone else edited\n", 0o644);
    let err = execute_write(&fs, &plan, false, 5, "r", 5)
        .await
        .unwrap_err();
    assert!(err.contains("被改动"), "{err}");
    assert_eq!(
        fs.get("/home/u/conf.txt").unwrap(),
        b"someone else edited\n"
    );
    assert!(fs
        .log()
        .iter()
        .all(|l| !l.starts_with("write ") && !l.starts_with("rename")));

    // 被删了
    fs.nodes.lock().unwrap().remove("/home/u/conf.txt");
    let err = execute_write(&fs, &plan, false, 5, "r", 5)
        .await
        .unwrap_err();
    assert!(err.contains("被删除"), "{err}");

    // 新建时别人抢先建了同名文件：不覆盖
    let fresh = prepare_write(&fs, "race.txt", "mine\n", true)
        .await
        .unwrap();
    fs.put("/home/u/race.txt", b"theirs\n", 0o644);
    let err = execute_write(&fs, &fresh, true, 5, "r", 5)
        .await
        .unwrap_err();
    assert!(err.contains("被创建"), "{err}");
    assert_eq!(fs.get("/home/u/race.txt").unwrap(), b"theirs\n");
}

#[tokio::test]
async fn same_second_backups_get_a_counter_instead_of_clobbering() {
    let fs = MemFs::new();
    fs.put("/home/u/c.txt", b"v1\n", 0o644);
    let p1 = prepare_write(&fs, "c.txt", "v2\n", false).await.unwrap();
    let r1 = execute_write(&fs, &p1, false, 100, "a", 5).await.unwrap();
    let p2 = prepare_write(&fs, "c.txt", "v3\n", false).await.unwrap();
    let r2 = execute_write(&fs, &p2, false, 100, "b", 5).await.unwrap();
    assert_ne!(r1.backup_path, r2.backup_path);
    assert_eq!(fs.get(r1.backup_path.as_deref().unwrap()).unwrap(), b"v1\n");
    assert_eq!(fs.get(r2.backup_path.as_deref().unwrap()).unwrap(), b"v2\n");
}

#[tokio::test]
async fn result_carries_the_danger_marker_for_system_paths() {
    let fs = MemFs::new();
    fs.put_dir("/etc");
    fs.put("/etc/motd", b"a\n", 0o644);
    let plan = prepare_write(&fs, "/etc/motd", "b\n", false).await.unwrap();
    let result = execute_write(&fs, &plan, false, 7, "r", 5).await.unwrap();
    assert!(result.dangerous);
    assert!(!result.danger_reasons.is_empty());
}

// ---------- 备份保留份数 ----------

#[test]
fn backup_names_are_parsed_strictly() {
    assert_eq!(
        backup_sort_key("conf.txt", "conf.txt.zssh-bak-20260101-120000"),
        Some(("20260101-120000".to_owned(), 1))
    );
    assert_eq!(
        backup_sort_key("conf.txt", "conf.txt.zssh-bak-20260101-120000-3"),
        Some(("20260101-120000".to_owned(), 3))
    );
    for bad in [
        // 别的文件
        "other.txt.zssh-bak-20260101-120000",
        "conf.txt.zssh-bak-20260101-120000.bak",
        "conf.txt.zssh-bak-20260101-120000-",
        "conf.txt.zssh-bak-20260101-1200",
        "conf.txt.zssh-bak-20261301-120000",
        "conf.txt.zssh-bak-20260101-240000",
        "conf.txt.zssh-bak-20260101-120000-x",
        "conf.txt.zssh-bak-20260101-120000-2-3",
        "conf.txt.zssh-old-20260101-120000",
        "my-conf.txt.zssh-bak-20260101-120000",
    ] {
        assert_eq!(backup_sort_key("conf.txt", bad), None, "{bad}");
    }
}

#[tokio::test]
async fn only_the_newest_backups_of_the_same_file_are_kept() {
    let fs = MemFs::new();
    fs.put("/home/u/conf.txt", b"v1\n", 0o644);
    // 别的文件、名字相似但不是 Z-SSH 生成的文件、目录：都不能删。
    fs.put(
        "/home/u/other.txt.zssh-bak-20200101-000000",
        b"other\n",
        0o644,
    );
    fs.put("/home/u/conf.txt.zssh-bak-not-a-time", b"keep\n", 0o644);
    fs.put_dir("/home/u/conf.txt.zssh-bak-20200101-000000");

    let mut backups = Vec::new();
    for (index, time) in [100i64, 200, 300, 400, 500, 600].into_iter().enumerate() {
        let plan = prepare_write(&fs, "conf.txt", &format!("v{}\n", index + 2), false)
            .await
            .unwrap();
        let result = execute_write(&fs, &plan, false, time, &format!("r{index}"), 5)
            .await
            .unwrap();
        if let Some(backup) = result.backup_path {
            backups.push(backup);
        }
        assert_eq!(result.pruned_backups, usize::from(index >= 5));
    }
    assert_eq!(backups.len(), 6);
    let names: Vec<String> = fs
        .names_in("/home/u")
        .iter()
        .map(|full| full.rsplit('/').next().unwrap_or("").to_owned())
        .collect();
    assert!(
        !names
            .iter()
            .any(|n| n == "conf.txt.zssh-bak-19700101-000140"),
        "最早的一份应该被删：{names:?}"
    );
    for kept in &backups[1..] {
        let name = kept.rsplit('/').next().unwrap();
        assert!(names.iter().any(|n| n == name), "{kept} 应该保留");
    }
    assert!(names
        .iter()
        .any(|n| n == "other.txt.zssh-bak-20200101-000000"));
    assert!(names.iter().any(|n| n == "conf.txt.zssh-bak-not-a-time"));
    assert!(names
        .iter()
        .any(|n| n == "conf.txt.zssh-bak-20200101-000000"));
}

#[tokio::test]
async fn cleanup_failure_does_not_fail_the_write_but_is_reported() {
    let fs = MemFs::new();
    fs.put("/home/u/conf.txt", b"v1\n", 0o644);
    for index in 0..3 {
        fs.put(
            &format!("/home/u/conf.txt.zssh-bak-2025010{}-000000", index + 1),
            b"old\n",
            0o644,
        );
    }
    *fs.fail_remove_containing.lock().unwrap() = Some("zssh-bak".to_owned());
    let plan = prepare_write(&fs, "conf.txt", "new\n", false)
        .await
        .unwrap();
    let result = execute_write(&fs, &plan, false, 600, "r", 1).await.unwrap();
    assert_eq!(fs.get("/home/u/conf.txt").unwrap(), b"new\n");
    assert_eq!(result.pruned_backups, 0);
    assert!(
        result
            .retention_note
            .as_deref()
            .unwrap_or("")
            .contains("旧备份清理失败"),
        "{:?}",
        result.retention_note
    );
}

// ---------- 属主 ----------

#[tokio::test]
async fn owner_is_set_on_the_temp_file_before_the_rename() {
    let fs = MemFs::new();
    fs.put("/home/u/root.conf", b"old\n", 0o644);
    fs.put_owner("/home/u/root.conf", 0, 0);
    *fs.new_file_owner.lock().unwrap() = Some((1000, 1000));
    let plan = prepare_write(&fs, "root.conf", "new\n", false)
        .await
        .unwrap();
    let result = execute_write(&fs, &plan, false, 5, "r", 5).await.unwrap();
    assert_eq!(fs.owner_of("/home/u/root.conf"), Some((0, 0)));
    assert!(result.owner_change.is_none());
    // 属主是在改名**之前**设在临时文件上的（设不了就还没替换，调用方可以先问用户）。
    let log = fs.log();
    let owner_at = log
        .iter()
        .position(|line| {
            line.starts_with("owner ") && line.contains("zssh-tmp") && line.ends_with(" 0 0")
        })
        .expect("应该先把临时文件的属主设成原文件的");
    let rename_at = log
        .iter()
        .position(|line| line.starts_with("rename "))
        .expect("应该改名");
    assert!(owner_at < rename_at, "{log:?}");
    assert!(
        !log.iter().any(|line| line == "owner /home/u/root.conf 0 0"),
        "改名之后不该再补设属主：{log:?}"
    );
}

#[tokio::test]
async fn owner_change_is_reported_when_it_cannot_be_restored() {
    let fs = MemFs::new();
    fs.put("/home/u/root.conf", b"old\n", 0o644);
    fs.put_owner("/home/u/root.conf", 0, 0);
    *fs.new_file_owner.lock().unwrap() = Some((1000, 1000));
    *fs.fail_set_owner.lock().unwrap() = true;
    let plan = prepare_write(&fs, "root.conf", "new\n", false)
        .await
        .unwrap();
    let result = execute_write(&fs, &plan, false, 5, "r", 5).await.unwrap();
    let change = result.owner_change.expect("要报告属主变了");
    assert_eq!(change.from, "uid 0 / gid 0");
    assert_eq!(change.to, "uid 1000 / gid 1000");
    assert!(change.error.contains("权限不够"), "{}", change.error);
    assert_eq!(fs.owner_of("/home/u/root.conf"), Some((1000, 1000)));
}

// ---------- 写文件：执行中途被作废（R-9 F12） ----------

/// 第 `refuse_from` 次过检查点起开始拒绝（1 起算）。
fn refusing_from(refuse_from: usize) -> impl Fn() -> Result<(), String> + Send + Sync {
    let calls = std::sync::atomic::AtomicUsize::new(0);
    move || {
        let call = calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if call >= refuse_from {
            Err("用户点了停止，这次操作已取消".to_owned())
        } else {
            Ok(())
        }
    }
}

fn no_leftovers(fs: &MemFs) -> bool {
    fs.names_in("/home/u")
        .iter()
        .all(|n| !n.contains("zssh-tmp") && !n.contains("zssh-bak"))
}

#[tokio::test]
async fn a_write_cancelled_before_it_starts_touches_nothing() {
    let fs = MemFs::new();
    fs.put("/home/u/conf.txt", b"old\n", 0o640);
    let plan = prepare_write(&fs, "conf.txt", "new\n", false)
        .await
        .unwrap();
    let err = execute_write_checked(&fs, &plan, false, 1_000_000_000, "r", 5, &refusing_from(1))
        .await
        .expect_err("开始前就作废");
    assert!(err.contains("停止") && err.contains("没有改动"), "{err}");
    assert_eq!(fs.get("/home/u/conf.txt").unwrap(), b"old\n");
    assert!(no_leftovers(&fs), "{:?}", fs.names_in("/home/u"));
}

#[tokio::test]
async fn a_write_cancelled_after_the_backup_removes_the_backup_and_keeps_the_file() {
    let fs = MemFs::new();
    fs.put("/home/u/conf.txt", b"old\n", 0o640);
    let plan = prepare_write(&fs, "conf.txt", "new\n", false)
        .await
        .unwrap();
    // 第 1 次（开始前）放行，第 2 次（备份做完）拒绝。
    let err = execute_write_checked(&fs, &plan, false, 1_000_000_000, "r", 5, &refusing_from(2))
        .await
        .expect_err("备份之后作废");
    assert!(err.contains("没有改动"), "{err}");
    assert_eq!(fs.get("/home/u/conf.txt").unwrap(), b"old\n");
    assert!(
        no_leftovers(&fs),
        "作废了就不该留下备份：{:?}",
        fs.names_in("/home/u")
    );
}

#[tokio::test]
async fn a_write_cancelled_right_before_the_rename_leaves_the_file_unchanged() {
    let fs = MemFs::new();
    fs.put("/home/u/conf.txt", b"old\n", 0o640);
    let plan = prepare_write(&fs, "conf.txt", "new\n", false)
        .await
        .unwrap();
    // 第 3 次（临时文件写好、改名之前）拒绝。
    let err = execute_write_checked(&fs, &plan, false, 1_000_000_000, "r", 5, &refusing_from(3))
        .await
        .expect_err("改名之前作废");
    assert!(err.contains("原文件没有改动"), "{err}");
    assert_eq!(fs.get("/home/u/conf.txt").unwrap(), b"old\n");
    assert!(no_leftovers(&fs), "{:?}", fs.names_in("/home/u"));
    assert!(
        !fs.log().iter().any(|l| l.starts_with("rename")),
        "没有改名：{:?}",
        fs.log()
    );
}

#[tokio::test]
async fn a_checkpoint_that_keeps_passing_writes_normally() {
    let fs = MemFs::new();
    fs.put("/home/u/conf.txt", b"old\n", 0o640);
    let plan = prepare_write(&fs, "conf.txt", "new\n", false)
        .await
        .unwrap();
    let result = execute_write_checked(&fs, &plan, false, 1_000_000_000, "r", 5, &|| Ok(()))
        .await
        .expect("一直放行就照常写");
    assert_eq!(fs.get("/home/u/conf.txt").unwrap(), b"new\n");
    assert!(result.backup_path.is_some());
}
