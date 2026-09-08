//! Observe-only recipe recording (semantic frames).
//!
//! This is **not** OS HID and not a wire `screenshot` op. The default
//! backend paints the semantic `UiTree` to SVG + PPM so CI can keep
//! artifacts without a display. Window-scoped pixels stay in
//! `scripts/record-window.sh` (macOS).

use std::fs;
use std::path::{Path, PathBuf};

use gpui_agent::tree::{UiNode, UiTree};
use serde::Serialize;

use crate::plan::Plan;
use crate::receipt::Receipt;

/// How `--record` is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordBackend {
    /// Snapshot the semantic tree after each step (works headless).
    Semantic,
    /// OS window capture — not implemented in-process. See `os_record_help`.
    Os,
}

impl RecordBackend {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "semantic" | "tree" => Ok(Self::Semantic),
            "os" | "window" => Ok(Self::Os),
            other => Err(format!(
                "unknown record backend `{other}` (want semantic or os)"
            )),
        }
    }
}

/// Error text for `--record-backend os` on platforms without a helper.
pub fn os_record_help() -> &'static str {
    "record backend `os` is not in-process (no ScreenCaptureKit / ffmpeg crate, \
     and this CLI never injects OS mouse/keyboard). On macOS, run \
     scripts/record-window.sh alongside the desktop `todo` window \
     (screencapture -l, observe-only). Headless/CI: omit --record-backend or \
     use --record-backend semantic. See docs/RECORDING.md"
}

/// Where frames go, and an optional later mux target (`.mp4`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordTarget {
    pub dir: PathBuf,
    pub mux_target: Option<PathBuf>,
}

/// Map `--record PATH` to a frames directory.
///
/// A video extension (`.mp4` / `.gif` / `.webm`) uses a sibling
/// `*.frames` directory; muxing is optional and external (`ffmpeg`).
pub fn resolve_record_target(path: &Path) -> RecordTarget {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(ext.as_str(), "mp4" | "gif" | "webm" | "mov") {
        let mut dir = path.to_path_buf();
        dir.set_extension(format!("{ext}.frames"));
        RecordTarget {
            dir,
            mux_target: Some(path.to_path_buf()),
        }
    } else {
        RecordTarget {
            dir: path.to_path_buf(),
            mux_target: None,
        }
    }
}

pub trait RecipeRecorder {
    fn on_start(&mut self, plan: &Plan) -> Result<(), String>;
    fn on_frame(
        &mut self,
        index: u32,
        step_id: &str,
        tree: Option<&UiTree>,
        step_ok: bool,
    ) -> Result<(), String>;
    fn on_finish(&mut self, receipt: &Receipt) -> Result<(), String>;
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordManifest {
    pub backend: &'static str,
    pub recipe: String,
    pub fingerprint: String,
    pub include_values: bool,
    pub frames: Vec<RecordFrameMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mux_target: Option<String>,
    pub mux_hint: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordFrameMeta {
    pub index: u32,
    pub step_id: String,
    pub ok: bool,
    pub svg: String,
    pub ppm: String,
}

/// Writes one SVG + PPM per step from the semantic tree.
pub struct SemanticRecorder {
    dir: PathBuf,
    include_values: bool,
    mux_target: Option<PathBuf>,
    recipe: String,
    fingerprint: String,
    frames: Vec<RecordFrameMeta>,
}

impl SemanticRecorder {
    pub fn create(target: RecordTarget, include_values: bool) -> Result<Self, String> {
        fs::create_dir_all(&target.dir)
            .map_err(|err| format!("{}: {err}", target.dir.display()))?;
        Ok(Self {
            dir: target.dir,
            include_values,
            mux_target: target.mux_target,
            recipe: String::new(),
            fingerprint: String::new(),
            frames: Vec::new(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl RecipeRecorder for SemanticRecorder {
    fn on_start(&mut self, plan: &Plan) -> Result<(), String> {
        self.recipe = plan.name.clone();
        self.fingerprint = plan.fingerprint.clone();
        let flag = self.dir.join("recording.flag");
        fs::write(&flag, b"1").map_err(|err| format!("{}: {err}", flag.display()))?;
        Ok(())
    }

    fn on_frame(
        &mut self,
        index: u32,
        step_id: &str,
        tree: Option<&UiTree>,
        step_ok: bool,
    ) -> Result<(), String> {
        let safe = sanitize_step_id(step_id);
        let stem = format!("{index:04}-{safe}");
        let painted = tree.map(|t| {
            if self.include_values {
                redact_passwords_only(t)
            } else {
                redact_values(t)
            }
        });
        let svg = tree_to_svg(painted.as_ref(), step_id, step_ok);
        let ppm = tree_to_ppm(painted.as_ref(), step_ok);
        let svg_name = format!("{stem}.svg");
        let ppm_name = format!("{stem}.ppm");
        fs::write(self.dir.join(&svg_name), svg)
            .map_err(|err| format!("write {svg_name}: {err}"))?;
        fs::write(self.dir.join(&ppm_name), ppm)
            .map_err(|err| format!("write {ppm_name}: {err}"))?;
        self.frames.push(RecordFrameMeta {
            index,
            step_id: step_id.to_string(),
            ok: step_ok,
            svg: svg_name,
            ppm: ppm_name,
        });
        Ok(())
    }

    fn on_finish(&mut self, receipt: &Receipt) -> Result<(), String> {
        let _ = fs::remove_file(self.dir.join("recording.flag"));
        let mux_target = self.mux_target.as_ref().map(|p| p.display().to_string());
        let mux_hint = match &mux_target {
            Some(out) => format!(
                "ffmpeg -y -framerate 2 -i {}/%04d-*.ppm {out}",
                self.dir.display()
            ),
            None => format!(
                "ffmpeg -y -framerate 2 -pattern_type glob -i '{}/*.ppm' {}/recipe-run.mp4",
                self.dir.display(),
                self.dir.display()
            ),
        };
        let manifest = RecordManifest {
            backend: "semantic",
            recipe: self.recipe.clone(),
            fingerprint: if self.fingerprint.is_empty() {
                receipt.fingerprint.clone()
            } else {
                self.fingerprint.clone()
            },
            include_values: self.include_values,
            frames: self.frames.clone(),
            mux_target,
            mux_hint,
        };
        let json = serde_json::to_string_pretty(&manifest).map_err(|err| err.to_string())?;
        let path = self.dir.join("manifest.json");
        fs::write(&path, json).map_err(|err| format!("{}: {err}", path.display()))?;
        Ok(())
    }
}

pub fn sanitize_step_id(id: &str) -> String {
    let mut out = String::new();
    for c in id.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

fn redact_values(tree: &UiTree) -> UiTree {
    let mut out = tree.clone();
    for node in &mut out.nodes {
        walk_mut(node, &mut |n| {
            if n.value.is_some() {
                n.value = Some("«redacted»".into());
            }
        });
    }
    out
}

fn redact_passwords_only(tree: &UiTree) -> UiTree {
    let mut out = tree.clone();
    for node in &mut out.nodes {
        walk_mut(node, &mut |n| {
            if n.role.eq_ignore_ascii_case("password") && n.value.is_some() {
                n.value = Some("«redacted»".into());
            }
        });
    }
    out
}

fn walk_mut(node: &mut UiNode, f: &mut impl FnMut(&mut UiNode)) {
    f(node);
    for child in &mut node.children {
        walk_mut(child, f);
    }
}

struct FrameRow {
    id: String,
    role: String,
    name: String,
    value: Option<String>,
    checked: Option<bool>,
}

fn tree_to_svg(tree: Option<&UiTree>, step_id: &str, step_ok: bool) -> String {
    let rows = flatten_rows(tree);
    let width = 640;
    let row_h = 28;
    let height = (rows.len() as u32 * row_h + 48).max(80);
    let status = if step_ok { "ok" } else { "fail" };
    let fill_bg = if step_ok { "#0f172a" } else { "#3f1d1d" };
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">\n\
         <rect width=\"100%\" height=\"100%\" fill=\"{fill_bg}\"/>\n\
         <text x=\"12\" y=\"22\" fill=\"#e2e8f0\" font-family=\"monospace\" font-size=\"14\">step {esc} ({status})</text>\n",
        esc = escape_xml(step_id)
    );
    for (i, row) in rows.iter().enumerate() {
        let y = 40 + i as u32 * row_h;
        let bar = role_color(&row.role);
        out.push_str(&format!(
            "<rect x=\"12\" y=\"{y}\" width=\"8\" height=\"22\" fill=\"{bar}\"/>\n\
             <text x=\"28\" y=\"{text_y}\" fill=\"#cbd5e1\" font-family=\"monospace\" font-size=\"12\">{label}</text>\n",
            text_y = y + 16,
            label = escape_xml(&row_label(
                &row.id,
                &row.role,
                &row.name,
                row.value.as_deref(),
                row.checked,
            )),
        ));
    }
    if rows.is_empty() {
        out.push_str(
            "<text x=\"12\" y=\"56\" fill=\"#94a3b8\" font-family=\"monospace\" font-size=\"12\">(no snapshot)</text>\n",
        );
    }
    out.push_str("</svg>\n");
    out
}

fn tree_to_ppm(tree: Option<&UiTree>, step_ok: bool) -> Vec<u8> {
    let rows = flatten_rows(tree);
    let width: u32 = 160;
    let row_h: u32 = 8;
    let height = (rows.len() as u32 * row_h + 8).max(16);
    let mut pixels = vec![0u8; (width * height * 3) as usize];
    let bg = if step_ok {
        [15u8, 23, 42]
    } else {
        [63, 29, 29]
    };
    for px in pixels.chunks_mut(3) {
        px.copy_from_slice(&bg);
    }
    for (i, row) in rows.iter().enumerate() {
        let color = role_rgb(&row.role, row.checked);
        let y0 = 4 + i as u32 * row_h;
        for y in y0..(y0 + row_h - 1).min(height) {
            for x in 4..(width - 4) {
                let idx = ((y * width + x) * 3) as usize;
                pixels[idx..idx + 3].copy_from_slice(&color);
            }
        }
    }
    let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
    out.extend_from_slice(&pixels);
    out
}

fn flatten_rows(tree: Option<&UiTree>) -> Vec<FrameRow> {
    let Some(tree) = tree else {
        return Vec::new();
    };
    tree.flatten()
        .into_iter()
        .map(|n| FrameRow {
            id: n.id.clone(),
            role: n.role.clone(),
            name: n.name.clone(),
            value: n.value.clone(),
            checked: n.checked,
        })
        .collect()
}

fn row_label(
    id: &str,
    role: &str,
    name: &str,
    value: Option<&str>,
    checked: Option<bool>,
) -> String {
    let mut s = format!("{id}  {role}  {name}");
    if let Some(v) = value {
        s.push_str("  value=");
        s.push_str(v);
    }
    if let Some(c) = checked {
        s.push_str(if c { "  checked" } else { "  unchecked" });
    }
    s
}

fn role_color(role: &str) -> &'static str {
    match role {
        "button" => "#38bdf8",
        "textbox" | "input" => "#a78bfa",
        "checkbox" | "switch" => "#34d399",
        "window" => "#fbbf24",
        _ => "#64748b",
    }
}

fn role_rgb(role: &str, checked: Option<bool>) -> [u8; 3] {
    if checked == Some(true) {
        return [52, 211, 153];
    }
    match role {
        "button" => [56, 189, 248],
        "textbox" | "input" => [167, 139, 250],
        "checkbox" | "switch" => [52, 211, 153],
        "window" => [251, 191, 36],
        _ => [100, 116, 139],
    }
}

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_agent::protocol::PlatformKind;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn sample_tree() -> UiTree {
        UiTree {
            app: "todo".into(),
            platform: PlatformKind::Headless,
            ready: true,
            nodes: vec![
                UiNode::new("todo-window", "window", "Agent Todo").with_child(
                    UiNode::new("todo-input", "textbox", "Draft")
                        .with_value("secret-draft")
                        .with_child(UiNode::new("todo-add", "button", "Add")),
                ),
            ],
        }
    }

    #[test]
    fn resolve_record_target_video_uses_sidecar_dir() {
        let t = resolve_record_target(Path::new("artifacts/recipe-run.mp4"));
        assert_eq!(t.dir, PathBuf::from("artifacts/recipe-run.mp4.frames"));
        assert_eq!(
            t.mux_target.as_deref(),
            Some(Path::new("artifacts/recipe-run.mp4"))
        );
        let dir = resolve_record_target(Path::new("artifacts/run"));
        assert_eq!(dir.dir, PathBuf::from("artifacts/run"));
        assert!(dir.mux_target.is_none());
    }

    #[test]
    fn backend_parse() {
        assert_eq!(
            RecordBackend::parse("semantic").unwrap(),
            RecordBackend::Semantic
        );
        assert_eq!(RecordBackend::parse("os").unwrap(), RecordBackend::Os);
        assert!(RecordBackend::parse("webcam").is_err());
    }

    #[test]
    fn svg_redacts_values_by_default() {
        let tree = sample_tree();
        let redacted = redact_values(&tree);
        let svg = tree_to_svg(Some(&redacted), "add", true);
        assert!(svg.contains("todo-input"));
        assert!(svg.contains("«redacted»"));
        assert!(!svg.contains("secret-draft"));
        assert!(svg.contains("step add"));
    }

    #[test]
    fn ppm_is_binary_p6() {
        let tree = sample_tree();
        let ppm = tree_to_ppm(Some(&tree), true);
        assert!(ppm.starts_with(b"P6\n"));
        assert!(ppm.len() > 20);
    }

    #[test]
    fn recorder_start_stop_writes_manifest() {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gpui-agent-record-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        let mut rec = SemanticRecorder::create(
            RecordTarget {
                dir: dir.clone(),
                mux_target: Some(PathBuf::from("out.mp4")),
            },
            false,
        )
        .unwrap();
        let plan = Plan {
            name: "demo".into(),
            app: None,
            steps: vec![],
            waves: vec![],
            effects: vec![],
            requires_yes: false,
            fingerprint: "abc".into(),
        };
        rec.on_start(&plan).unwrap();
        assert!(dir.join("recording.flag").exists());
        rec.on_frame(0, "wait", Some(&sample_tree()), true).unwrap();
        rec.on_frame(1, "add!", None, false).unwrap();
        let receipt = Receipt {
            ok: false,
            recipe: "demo".into(),
            fingerprint: "abc".into(),
            session_reused: true,
            steps: vec![],
            screenshots: vec![],
            elapsed_ms: 1,
        };
        rec.on_finish(&receipt).unwrap();
        assert!(!dir.join("recording.flag").exists());
        let manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["backend"], "semantic");
        assert_eq!(manifest["frames"].as_array().unwrap().len(), 2);
        assert!(dir.join("0000-wait.svg").exists());
        assert!(dir.join("0001-add_.ppm").exists());
        let svg = fs::read_to_string(dir.join("0000-wait.svg")).unwrap();
        assert!(svg.contains("«redacted»"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn os_help_mentions_script_and_no_hid() {
        let help = os_record_help();
        assert!(help.contains("record-window.sh"));
        assert!(help.contains("semantic"));
        assert!(help.contains("never injects"));
    }
}
