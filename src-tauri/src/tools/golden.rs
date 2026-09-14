//! golden 对照测试：用 Rust 实现跑 `tools/golden/cases.json` 的同一批用例，
//! 与 `tools/golden/expected/*.json`（由 Node 运行时录制）逐字节比对。
//!
//! 跑法：`pnpm tool:golden`（录制 + 对照），或 `cargo test --offline tools::golden -- --nocapture`。
//! 语料、fixture 布局与断言模式都在 cases.json 里，两侧共用同一份声明。

use crate::tools;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::id;

fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tools/golden")
}

fn make_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    return std::os::windows::fs::symlink_file(target, link);
    #[allow(unreachable_code)]
    Err(std::io::Error::other("本平台不支持符号链接"))
}

/// 与 record.mjs 同语义地铺 fixture（铺进 `<tmp>/template`）。返回 (临时根目录, 符号链接是否可用)
fn build_fixtures(files: &Value, tmp: &Path) -> bool {
    let _ = fs::remove_dir_all(tmp);
    fs::create_dir_all(tmp).expect("创建临时目录失败");
    let obj = files.as_object().expect("cases.json.files 必须是对象");
    let mut symlink_ok = true;
    for (rel, spec) in obj {
        let full = tmp.join("template").join(rel);
        if let Some(parent) = full.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match spec {
            Value::String(text) => {
                fs::write(&full, text).expect("写 fixture 失败");
            }
            Value::Object(o) => {
                if let Some(t) = o.get("repeat").and_then(|v| v.as_array()) {
                    let unit = t[0].as_str().unwrap_or_default();
                    let times = t[1].as_u64().unwrap_or(0) as usize;
                    fs::write(&full, unit.repeat(times)).expect("写 fixture 失败");
                } else if let Some(t) = o.get("linesOf").and_then(|v| v.as_array()) {
                    let prefix = t[0].as_str().unwrap_or_default();
                    let count = t[1].as_u64().unwrap_or(0);
                    let body = (1..=count).map(|i| format!("{prefix} {i}")).collect::<Vec<_>>().join("\n");
                    fs::write(&full, format!("{body}\n")).expect("写 fixture 失败");
                } else if let Some(bytes) = o.get("bytes").and_then(|v| v.as_array()) {
                    let v: Vec<u8> = bytes.iter().filter_map(|b| b.as_u64().map(|x| x as u8)).collect();
                    fs::write(&full, v).expect("写 fixture 失败");
                } else if let Some(target) = o.get("symlink").and_then(|v| v.as_str()) {
                    // 与 Node 端一样：失败（如 Windows 无权限）就把符号链接类用例整批跳过
                    if make_symlink(Path::new(target), &full).is_err() {
                        symlink_ok = false;
                    }
                } else {
                    panic!("fixture {rel} 的声明无法识别");
                }
            }
            _ => panic!("fixture {rel} 的声明无法识别"),
        }
    }
    symlink_ok
}

/// 拷树（软链按原样重建）：写类工具会改文件，每个用例前必须复位，否则基线不可重放
fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        let ft = e.file_type()?;
        if ft.is_symlink() {
            let target = fs::read_link(e.path())?;
            let _ = fs::remove_file(&to);
            make_symlink(&target, &to)?;
        } else if ft.is_dir() {
            copy_tree(&e.path(), &to)?;
        } else {
            fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}

fn reset_workspace(tmp: &Path, workspace_rel: &str) -> PathBuf {
    let work = tmp.join("work");
    let _ = fs::remove_dir_all(&work);
    copy_tree(&tmp.join("template"), &work).expect("复位工作区失败");
    work.join(workspace_rel)
}

/// 落盘结果同样入对照：只看返回文案会漏掉「文案对、文件写坏」这类漂移
fn inspect_files(workspace: &Path, list: &[String]) -> Value {
    let mut map = serde_json::Map::new();
    for rel in list {
        let p = workspace.join(rel);
        let v = match fs::symlink_metadata(&p) {
            Err(_) => Value::Null,
            Ok(md) if md.is_dir() => json!("<目录>"),
            Ok(md) if md.file_type().is_symlink() => json!(format!(
                "<软链→{}>",
                fs::read_link(&p).map(|t| t.to_string_lossy().into_owned()).unwrap_or_default()
            )),
            Ok(_) => match fs::read(&p) {
                Ok(b) => Value::String(String::from_utf8_lossy(&b).into_owned()),
                Err(e) => json!(format!("<读失败 {e}>")),
            },
        };
        map.insert(rel.clone(), v);
    }
    Value::Object(map)
}

/// 打印首个差异行，方便判断是格式漂移还是文案差异
fn first_diff(a: &str, b: &str) -> String {
    let al: Vec<&str> = a.split('\n').collect();
    let bl: Vec<&str> = b.split('\n').collect();
    for i in 0..al.len().max(bl.len()) {
        let (x, y) = (
            al.get(i).copied().unwrap_or("<无>"),
            bl.get(i).copied().unwrap_or("<无>"),
        );
        if x != y {
            return format!("第 {} 行不同\n  node: {x}\n  rust: {y}", i + 1);
        }
    }
    "仅整体长度不同".to_string()
}

#[test]
fn golden_matches_node_baseline() {
    let dir = corpus_dir();
    let raw = fs::read_to_string(dir.join("cases.json"))
        .expect("读 tools/golden/cases.json 失败");
    let corpus: Value = serde_json::from_str(&raw).expect("cases.json 不是合法 JSON");
    let workspace_rel = corpus["workspace"].as_str().unwrap_or("repo").to_string();
    let tmp = std::env::temp_dir().join(format!("qs-golden-rust-{}", id()));
    let symlink_ok = build_fixtures(&corpus["files"].clone(), &tmp);

    let gaps = corpus["rustKnownGaps"].as_object().cloned().unwrap_or_default();
    let mut fails: Vec<String> = Vec::new();
    let mut checked = 0usize;
    let mut skipped = 0usize;
    let mut gap_hits = 0usize;

    for case in corpus["cases"].as_array().unwrap() {
        let cid = case["id"].as_str().unwrap_or("?").to_string();
        let exp_path = dir.join("expected").join(format!("{cid}.json"));
        let Ok(exp_raw) = fs::read_to_string(&exp_path) else {
            fails.push(format!("{cid}: 缺期望文件（先跑 pnpm tool:record）"));
            continue;
        };
        let exp: Value = serde_json::from_str(&exp_raw).expect("期望文件不是合法 JSON");
        if exp.get("skipped").is_some() {
            skipped += 1;
            continue;
        }
        if case["requiresSymlink"].as_bool().unwrap_or(false) && !symlink_ok {
            println!("  ~ {cid}: 本平台建不了符号链接，跳过");
            skipped += 1;
            continue;
        }
        let tool = case["tool"].as_str().unwrap_or("");
        let args = case.get("args").cloned().unwrap_or_else(|| json!({}));
        let workspace = reset_workspace(&tmp, &workspace_rel);
        let want_ok = exp["ok"].as_bool().unwrap_or(false);
        let want = exp["text"].as_str().unwrap_or_default();
        // assert=error 的用例只比对「是否失败」：OS 错误文案（ENOENT… / os error 2）两边不可能一致
        let loose = exp["assert"].as_str() == Some("error");
        let (got_ok, got) = match tools::run(workspace.to_str().unwrap(), tool, &args) {
            Ok(t) => (true, t),
            Err(e) => (false, e),
        };
        // 落盘对照：写类用例即使文案一致，文件写错也算失败
        let inspect: Vec<String> = case["inspect"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if !inspect.is_empty() {
            let got_files = inspect_files(&workspace, &inspect);
            let want_files = exp.get("files").cloned().unwrap_or_else(|| json!({}));
            checked += 1;
            if got_files == want_files {
                println!("  ✓ {cid}（落盘一致）");
            } else if gaps.contains_key(&cid) {
                gap_hits += 1;
                println!("  ! {cid} 落盘已知缺口");
            } else {
                fails.push(format!(
                    "{cid}: 落盘不一致\n    node: {}\n    rust: {}",
                    serde_json::to_string(&want_files).unwrap_or_default(),
                    serde_json::to_string(&got_files).unwrap_or_default()
                ));
            }
        }
        checked += 1;
        if got_ok != want_ok {
            if gaps.contains_key(&cid) {
                gap_hits += 1;
                println!("  ! {cid} 已知缺口：rust {}「{got}」｜node {}「{want}」",
                    if got_ok { "成功" } else { "失败" }, if want_ok { "成功" } else { "失败" });
            } else {
                fails.push(format!("{cid}: 成败位不一致 rust={}（{got}）｜node={}（{want}）",
                    if got_ok { "ok" } else { "err" }, if want_ok { "ok" } else { "err" }));
            }
        } else if loose && !got_ok {
            println!("  ✓ {cid}（都失败）node: {want}｜rust: {got}");
        } else if got == want {
            println!("  ✓ {cid}");
        } else {
            fails.push(format!("{cid}: 输出不一致（{} vs {} 字符）\n    {}",
                got.chars().count(), want.chars().count(), first_diff(want, &got)));
        }
    }

    let _ = fs::remove_dir_all(&tmp);
    println!(
        "\nRust 引擎 golden：{checked} 条已比对，{skipped} 条跳过，{gap_hits} 条命中已知缺口"
    );
    assert!(
        fails.is_empty(),
        "golden 未通过 {} 条：\n{}",
        fails.len(),
        fails.join("\n")
    );
}

/// 未移植的工具必须明确报错，而不是静默走别的路径；已移植的不得混进回落名单
#[test]
fn unmigrated_tools_report_clearly() {
    for name in ["bash"] {
        assert!(!tools::supports(name), "{name} 已移植，请同步更新本测试与回落名单");
        let e = tools::run("/tmp", name, &json!({})).unwrap_err();
        assert!(e.contains("尚未移植"), "{name} 的报错文案不该变：{e}");
    }
    for name in ["list", "read", "glob", "grep", "write", "edit", "patch", "todowrite", "skill"] {
        assert!(tools::supports(name), "{name} 应已接入 Rust 分发");
    }
    // 缺 workspace 的口径与 Node 一致：todowrite 不需要，其余都要
    assert!(tools::run("", "read", &json!({})).unwrap_err().contains("缺少 workspace"));
    assert!(tools::run("", "todowrite", &json!({ "todos": [] })).unwrap().contains("已清空"));
}

/// 在**真实仓库**上跑一遍只读工具，肉眼检查输出（比 fixture 更能看出格式漂移）。
/// 默认忽略：`cargo test -- --ignored --nocapture tools::golden`
#[test]
#[ignore]
fn smoke_on_this_repo() {
    let ws = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .to_string_lossy()
        .to_string();
    for (tool, args) in [
        ("list", json!({ "path": "src" })),
        ("read", json!({ "filePath": "package.json", "limit": 8 })),
        ("glob", json!({ "pattern": "src/lib/*.js" })),
        ("grep", json!({ "pattern": "CONTEXT_TOKEN_BUDGET", "include": "*.js" })),
    ] {
        match tools::run(&ws, tool, &args) {
            Ok(t) => println!("── {tool} {args}\n{t}\n"),
            Err(e) => println!("── {tool} {args}\n错误：{e}\n"),
        }
    }
}
