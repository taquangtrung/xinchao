//! `xinchao` command-line tool.
//!
//! Human-facing entry point for enrollment, testing, and diagnostics. The CLI is
//! a client of [`xinchao`]; it never participates in the PAM authentication
//! path. Diagnostics (M1), face detection and embedding via `test` (M2),
//! enrollment management via `add`/`list`/`remove` (M3), and model installation
//! via `install-models` (M5) are implemented.

use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use clap::Args;
use clap::Parser;
use clap::Subcommand;
use xinchao::auth::verify;
use xinchao::auth::verify::Verifier;
use xinchao::capture::camera;
use xinchao::capture::camera::DeviceInfo;
use xinchao::capture::emitter;
use xinchao::config;
use xinchao::config::Config;
use xinchao::pam::conf as pamconf;
use xinchao::recognition::detect;
use xinchao::recognition::embed;
use xinchao::recognition::embed::RecognitionModel;
use xinchao::recognition::models;
use xinchao::recognition::recognize;
use xinchao::recognition::recognize::Embedding;
use xinchao::store;
use xinchao::store::Enrollment;

mod unlock_daemon;

// Constants

/// Default capture width for the `diagnose --capture` debug frame.
const CAPTURE_WIDTH: u32 = 640;

/// Default capture height for the `diagnose --capture` debug frame.
const CAPTURE_HEIGHT: u32 = 480;

/// Hard cap on how long a debug capture may take.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(4);

/// Mean luma below which the IR illuminator is almost certainly not firing.
const IR_DARK_MEAN: f64 = 8.0;

/// Number of frames captured per enrollment when not overridden.
const DEFAULT_ENROLL_FRAMES: u32 = 5;

// Data Structures

/// Top-level CLI parser.
#[derive(Debug, Parser)]
#[command(name = "xinchao", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Available subcommands, mirroring the milestone roadmap.
#[derive(Debug, Subcommand)]
enum Command {
    /// Enroll the current or target user's face by capturing several frames.
    Add(AddArgs),
    /// Show and validate the effective configuration.
    Config(ConfigArgs),
    /// List V4L2 devices, identify the IR node, and check config/permissions.
    Diagnose(DiagnoseArgs),
    /// Probe (and optionally activate) the camera's IR illuminator.
    EnableIr(EnableIrArgs),
    /// Download and checksum-verify the recognition and detector models.
    InstallModels(InstallModelsArgs),
    /// List enrolled users.
    List(ListArgs),
    /// Enable, disable, or show face unlock for a PAM service.
    Pam(PamArgs),
    /// Remove a user's enrollment.
    Remove(RemoveArgs),
    /// Compute a face embedding from an image or a live frame (no auth side effects).
    Test(TestArgs),
    /// Run the resident face-unlock daemon (watches logind; unlocks on a match).
    UnlockDaemon(UnlockDaemonArgs),
}

/// Options for the `add` subcommand.
#[derive(Args, Debug)]
struct AddArgs {
    /// Node to capture from; defaults to the auto-detected IR node.
    #[arg(long, value_name = "PATH")]
    device: Option<PathBuf>,
    /// Number of frames to capture and embed.
    #[arg(long, default_value_t = DEFAULT_ENROLL_FRAMES, value_name = "N")]
    frames: u32,
    /// Recognition model: arcface (accurate) or sface (fast).
    #[arg(long, value_parser = parse_model, default_value = "arcface")]
    model: RecognitionModel,
    /// Directory to cache the recognition model in.
    #[arg(long, value_name = "PATH")]
    models_dir: Option<PathBuf>,
    /// Directory to store enrollments in; defaults to the per-user store path.
    #[arg(long, value_name = "PATH")]
    store_dir: Option<PathBuf>,
    /// User to enroll; defaults to the current user ($USER).
    #[arg(long, value_name = "NAME")]
    user: Option<String>,
}

/// Options for the `config` subcommand.
#[derive(Args, Debug)]
struct ConfigArgs {
    /// Config file to read or modify; defaults to the system path.
    #[arg(long, value_name = "PATH")]
    path: Option<PathBuf>,
    /// Set the recognition model (arcface or sface) and exit (needs root).
    #[arg(long, value_parser = parse_model)]
    set_model: Option<RecognitionModel>,
}

/// Options for the `install-models` subcommand.
#[derive(Args, Debug)]
struct InstallModelsArgs {
    /// Directory to install the models into; defaults to the system models path.
    #[arg(long, value_name = "PATH")]
    dir: Option<PathBuf>,
}

/// Options for the `list` subcommand.
#[derive(Args, Debug)]
struct ListArgs {
    /// Directory to read enrollments from; defaults to the per-user store path.
    #[arg(long, value_name = "PATH")]
    store_dir: Option<PathBuf>,
}

/// Options for the `pam` subcommand.
#[derive(Args, Debug)]
struct PamArgs {
    #[command(subcommand)]
    action: PamAction,
}

/// What to do with a service's face-unlock drop-in.
#[derive(Debug, Subcommand)]
enum PamAction {
    /// Disable face unlock for a service.
    Disable(PamServiceArgs),
    /// Enable face unlock for a service (adds `auth sufficient pam_xinchao.so`).
    Enable(PamServiceArgs),
    /// Show whether each supported service has face unlock enabled.
    Status,
}

/// Options naming a single PAM service.
#[derive(Args, Debug)]
struct PamServiceArgs {
    /// Service to modify: `sudo` or `polkit-1`.
    #[arg(long, value_name = "SERVICE")]
    service: String,
}

/// Options for the `remove` subcommand.
#[derive(Args, Debug)]
struct RemoveArgs {
    /// Directory to remove the enrollment from; defaults to the per-user store path.
    #[arg(long, value_name = "PATH")]
    store_dir: Option<PathBuf>,
    /// User whose enrollment to remove; defaults to the current user ($USER).
    #[arg(long, value_name = "NAME")]
    user: Option<String>,
}

/// Options for the `diagnose` subcommand.
#[derive(Args, Debug)]
struct DiagnoseArgs {
    /// Capture one frame and save it here as a PNG (proves capture works).
    #[arg(long, value_name = "PATH")]
    capture: Option<PathBuf>,
    /// Node to capture from; defaults to the auto-detected IR node.
    #[arg(long, value_name = "PATH")]
    device: Option<PathBuf>,
}

/// Options for the `test` subcommand.
#[derive(Args, Debug)]
struct TestArgs {
    /// Second image to compare against; prints cosine distance and a decision.
    #[arg(long, value_name = "PATH")]
    against: Option<PathBuf>,
    /// Node to capture from when no --image is given; defaults to the IR node.
    #[arg(long, value_name = "PATH")]
    device: Option<PathBuf>,
    /// Embed this image file instead of capturing a live frame.
    #[arg(long, value_name = "PATH")]
    image: Option<PathBuf>,
    /// Recognition model: arcface (accurate) or sface (fast).
    #[arg(long, value_parser = parse_model, default_value = "arcface")]
    model: RecognitionModel,
    /// Directory to cache the recognition model in.
    #[arg(long, value_name = "PATH")]
    models_dir: Option<PathBuf>,
    /// Directory to read enrollments from; defaults to the per-user store path.
    #[arg(long, value_name = "PATH")]
    store_dir: Option<PathBuf>,
    /// Match the probe against this enrolled user's stored embeddings.
    #[arg(long, value_name = "NAME")]
    user: Option<String>,
}

/// Options for the `unlock-daemon` subcommand.
#[derive(Args, Debug)]
struct UnlockDaemonArgs {
    /// Config file to read; defaults to the system path.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Milliseconds to poll the session lock state while idle.
    #[arg(long, default_value_t = 800, value_name = "MS")]
    poll_ms: u64,
    /// Milliseconds to wait after a non-matching scan before retrying.
    #[arg(long, default_value_t = 1500, value_name = "MS")]
    retry_ms: u64,
    /// Directory holding enrollments; defaults to the system store path.
    #[arg(long, value_name = "PATH")]
    store_dir: Option<PathBuf>,
    /// User whose locked graphical session to unlock on a face match.
    #[arg(long, value_name = "NAME")]
    user: String,
}

/// Options for the `enable-ir` subcommand.
#[derive(Args, Debug)]
struct EnableIrArgs {
    /// Attempt to activate the emitter (writes vendor control payloads) and,
    /// on success, persist the working payload for boot replay.
    #[arg(long)]
    apply: bool,
    /// Replay the saved activation and exit, without probing or sweeping; used
    /// by the boot/resume service to re-arm the emitter before any face auth.
    #[arg(long, conflicts_with = "apply")]
    boot: bool,
    /// Node to operate on; defaults to the auto-detected IR node.
    #[arg(long, value_name = "PATH")]
    device: Option<PathBuf>,
}

// Functions

fn main() -> Result<()> {
    let cli = Cli::parse();
    run(cli.command)
}

fn run(command: Command) -> Result<()> {
    match command {
        Command::Add(args) => add(args),
        Command::Config(args) => show_config(args),
        Command::Diagnose(args) => diagnose(args),
        Command::EnableIr(args) => enable_ir(args),
        Command::InstallModels(args) => install_models(args),
        Command::List(args) => list(args),
        Command::Pam(args) => pam(args),
        Command::Remove(args) => remove(args),
        Command::Test(args) => test(args),
        Command::UnlockDaemon(args) => unlock_daemon_cmd(args),
    }
}

/// Builds the daemon options from CLI flags (system paths by default) and serves.
fn unlock_daemon_cmd(args: UnlockDaemonArgs) -> Result<()> {
    let opts = unlock_daemon::Options {
        config_path: args
            .config
            .unwrap_or_else(|| PathBuf::from(config::DEFAULT_PATH)),
        poll: Duration::from_millis(args.poll_ms),
        retry: Duration::from_millis(args.retry_ms),
        store_dir: args
            .store_dir
            .unwrap_or_else(|| PathBuf::from(store::DEFAULT_DIR)),
        user: args.user,
    };
    unlock_daemon::serve(opts)
}

/// Lists V4L2 nodes, marks the detected IR node, and optionally captures a frame.
fn diagnose(args: DiagnoseArgs) -> Result<()> {
    let devices = camera::enumerate().context("enumerating V4L2 devices")?;
    if devices.is_empty() {
        println!("No /dev/video* devices found.");
        return Ok(());
    }

    let ir = camera::detect_ir(&devices);
    for (index, device) in devices.iter().enumerate() {
        print_device(index, device, ir == Some(index));
    }
    print_summary(&devices, ir);

    if let Some(output) = args.capture {
        capture_debug_frame(&devices, ir, args.device, &output)?;
    }
    Ok(())
}

/// Prints one device's path, identity, and advertised formats.
fn print_device(index: usize, device: &DeviceInfo, is_ir: bool) {
    let marker = if is_ir { "  <- IR node" } else { "" };
    println!("[{index}] {}{marker}", device.path.display());
    if let Some(by_id) = &device.by_id {
        println!("      by-id  : {by_id}");
    }
    if let Some(error) = &device.error {
        println!("      error  : {error}");
        return;
    }
    println!("      driver : {}", device.driver);
    println!("      card   : {}", device.card);
    println!("      capture: {}", device.is_capture);
    if !device.formats.is_empty() {
        let formats: Vec<String> = device
            .formats
            .iter()
            .map(|f| format!("{} ({})", f.fourcc, f.description))
            .collect();
        println!("      formats: {}", formats.join(", "));
    }
}

/// Prints the IR-detection verdict and a permission hint when relevant.
fn print_summary(devices: &[DeviceInfo], ir: Option<usize>) {
    match ir {
        Some(index) => println!("\nDetected IR node: {}", devices[index].path.display()),
        None => println!("\nNo IR node detected."),
    }
    let permission_blocked = devices
        .iter()
        .any(|d| d.error.as_deref().is_some_and(|e| e.contains("denied")));
    if permission_blocked {
        println!("Some nodes could not be opened. Add your user to the `video` group:");
        println!("    sudo usermod -aG video $USER   (then re-login)");
    }
}

/// Captures a single frame from the chosen node and writes it as a PNG.
fn capture_debug_frame(
    devices: &[DeviceInfo],
    ir: Option<usize>,
    device: Option<PathBuf>,
    output: &Path,
) -> Result<()> {
    let path = device
        .or_else(|| ir.map(|i| devices[i].path.clone()))
        .context("no IR node detected; pass --device to choose one")?;
    println!("\nCapturing from {} ...", path.display());
    let frame = camera::capture_frame(&path, CAPTURE_WIDTH, CAPTURE_HEIGHT, CAPTURE_TIMEOUT)
        .context("capturing debug frame")?;
    camera::save_debug_png(&frame, output).context("saving debug PNG")?;
    println!(
        "Saved {}x{} {} frame to {}",
        frame.width,
        frame.height,
        frame.fourcc,
        output.display()
    );

    let stats = frame.brightness().context("measuring frame brightness")?;
    println!(
        "Brightness: mean {:.1}, min {}, max {} (0-255)",
        stats.mean, stats.min, stats.max
    );
    if stats.mean < IR_DARK_MEAN {
        println!(
            "Frame is nearly black: the IR illuminator is likely off. Many UVC IR\n\
             cameras need an activation packet (see linux-enable-ir-emitter). Enabling\n\
             the emitter is the next step before recognition can work."
        );
    }
    Ok(())
}

/// Computes a face embedding from an image or a live frame, with no auth effect.
fn test(args: TestArgs) -> Result<()> {
    let models_dir = args.models_dir.unwrap_or_else(default_models_dir);
    let mut verifier = build_verifier(&models_dir, args.model)?;

    let probe = probe_embedding(&mut verifier, args.image.as_deref(), args.device)?;
    let threshold = Config::default().recognition.threshold as f32;
    if let Some(other) = args.against {
        let (other_embedding, _) = embed_image_file(&mut verifier, &other)?;
        let distance = probe.cosine_distance(&other_embedding);
        let verdict = if distance <= threshold {
            "MATCH"
        } else {
            "no match"
        };
        println!("\nCosine distance: {distance:.4} (threshold {threshold:.2})");
        println!("Decision: {verdict}");
    } else if let Some(user) = args.user {
        match_enrolled(&probe, &user, args.store_dir, threshold)?;
    } else {
        let values = probe.as_slice();
        let preview: Vec<String> = values.iter().take(5).map(|v| format!("{v:.3}")).collect();
        println!(
            "\nEmbedding: dim {}, L2 norm {:.4}",
            probe.dim(),
            l2_norm(values)
        );
        println!("First values: [{}]", preview.join(", "));
    }
    Ok(())
}

/// Compares a probe against a user's enrolled embeddings and prints the verdict.
fn match_enrolled(
    probe: &Embedding,
    user: &str,
    store_dir: Option<PathBuf>,
    threshold: f32,
) -> Result<()> {
    let dir = store_dir.unwrap_or_else(default_store_dir);
    let enrollment = store::load(&dir, user)
        .with_context(|| format!("loading enrollment for {user} from {}", dir.display()))?;
    let decision = recognize::decide(probe, enrollment.embeddings(), threshold)
        .context("enrollment has no embeddings")?;
    let verdict = if decision.accepted {
        "MATCH"
    } else {
        "no match"
    };
    println!(
        "\nNearest of {} enrolled embeddings: distance {:.4} (threshold {threshold:.2})",
        enrollment.embeddings().len(),
        decision.distance
    );
    println!("Decision: {verdict} for user {user}");
    Ok(())
}

/// Ensures the recognition and detector models, then loads them into a verifier.
fn build_verifier(models_dir: &Path, recognition: RecognitionModel) -> Result<Verifier> {
    let spec = recognition.spec();
    let model = models::ensure(spec, models_dir)
        .with_context(|| format!("ensuring recognition model in {}", models_dir.display()))?;
    let detector = models::ensure(&detect::ULTRAFACE, models_dir)
        .with_context(|| format!("ensuring detector model in {}", models_dir.display()))?;
    println!(
        "Recognition model: {} (license: {})",
        model.display(),
        spec.license
    );
    println!(
        "Detector model   : {} (license: {})",
        detector.display(),
        detect::ULTRAFACE.license
    );
    Verifier::load(&model, Some(&detector)).context("loading models")
}

/// Parses a `--model` value into a [`RecognitionModel`].
fn parse_model(value: &str) -> std::result::Result<RecognitionModel, String> {
    match value.to_lowercase().as_str() {
        "arcface" => Ok(RecognitionModel::Arcface),
        "sface" => Ok(RecognitionModel::Sface),
        other => Err(format!(
            "unknown model {other:?}; use \"arcface\" or \"sface\""
        )),
    }
}

/// Produces an embedding from an image file, or from a freshly captured frame.
fn probe_embedding(
    verifier: &mut Verifier,
    image: Option<&Path>,
    device: Option<PathBuf>,
) -> Result<Embedding> {
    if let Some(path) = image {
        println!("Embedding image {}", path.display());
        let (embedding, found) = embed_image_file(verifier, path)?;
        report_face(found, verifier.has_detector());
        return Ok(embedding);
    }
    let node = resolve_ir_node(device)?;
    println!("Capturing from {} ...", node.display());
    let frame = camera::capture_frame(&node, CAPTURE_WIDTH, CAPTURE_HEIGHT, CAPTURE_TIMEOUT)
        .context("capturing frame")?;
    let stats = frame.brightness().context("measuring frame brightness")?;
    if stats.mean < IR_DARK_MEAN {
        println!(
            "Warning: frame mean luma {:.1}; the IR emitter is likely off, so this\n\
             embedding will not be meaningful for recognition.",
            stats.mean
        );
    }
    let image = verify::frame_to_image(&frame).context("converting frame")?;
    let prepared = verifier.prepare(&image).context("detecting face")?;
    let found = prepared.is_some();
    report_face(found, verifier.has_detector());
    let face = prepared.unwrap_or(image);
    verifier.embed(&face).context("embedding frame")
}

/// Embeds an image file, detecting and cropping a face first when one is found.
///
/// Returns the embedding and whether a face was detected.
fn embed_image_file(verifier: &mut Verifier, path: &Path) -> Result<(Embedding, bool)> {
    let image = verify::load_image(path).with_context(|| format!("reading {}", path.display()))?;
    let prepared = verifier.prepare(&image).context("detecting face")?;
    let found = prepared.is_some();
    let face = prepared.unwrap_or(image);
    let embedding = verifier
        .embed(&face)
        .with_context(|| format!("embedding {}", path.display()))?;
    Ok((embedding, found))
}

/// Notes whether a face was detected, when a detector is in use.
fn report_face(found: bool, has_detector: bool) {
    if !has_detector {
        return;
    }
    if found {
        println!("Face detected; embedding the cropped face.");
    } else {
        println!("No face detected; embedding the whole frame.");
    }
}

/// Downloads and verifies all models into the system (or given) models directory.
fn install_models(args: InstallModelsArgs) -> Result<()> {
    let dir = args
        .dir
        .unwrap_or_else(|| PathBuf::from(store::DEFAULT_DIR));
    for spec in [&embed::ARCFACE, &embed::SFACE, &detect::ULTRAFACE] {
        let path = models::ensure(spec, &dir)
            .with_context(|| format!("installing {} into {}", spec.file_name, dir.display()))?;
        println!("  {} (license: {})", path.display(), spec.license);
    }
    println!(
        "\nModels installed and checksum-verified in {}.",
        dir.display()
    );
    Ok(())
}

/// Euclidean norm of an embedding, a quick stability/sanity indicator.
fn l2_norm(values: &[f32]) -> f32 {
    values.iter().map(|v| v * v).sum::<f32>().sqrt()
}

/// Default directory for cached recognition models when none is given.
fn default_models_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".cache/xinchao/models");
    }
    PathBuf::from("/tmp/xinchao/models")
}

/// Enrolls a user by capturing several frames and storing their embeddings.
fn add(args: AddArgs) -> Result<()> {
    let user = resolve_user(args.user)?;
    let store_dir = args.store_dir.unwrap_or_else(default_store_dir);
    let models_dir = args.models_dir.unwrap_or_else(default_models_dir);
    if args.frames == 0 {
        anyhow::bail!("--frames must be at least 1");
    }

    let mut verifier = build_verifier(&models_dir, args.model)?;
    let node = resolve_ir_node(args.device)?;

    println!(
        "Enrolling {user}: capturing {} frames from {} ...",
        args.frames,
        node.display()
    );
    let mut embeddings = Vec::with_capacity(args.frames as usize);
    let mut dark = 0u32;
    let mut faceless = 0u32;
    for index in 0..args.frames {
        let frame = camera::capture_frame(&node, CAPTURE_WIDTH, CAPTURE_HEIGHT, CAPTURE_TIMEOUT)
            .with_context(|| format!("capturing frame {}", index + 1))?;
        let stats = frame.brightness().context("measuring frame brightness")?;
        if stats.mean < IR_DARK_MEAN {
            dark += 1;
        }
        let image = verify::frame_to_image(&frame).context("converting frame")?;
        let prepared = verifier.prepare(&image).context("detecting face")?;
        let found = prepared.is_some();
        if !found {
            faceless += 1;
        }
        let face = prepared.unwrap_or(image);
        embeddings.push(verifier.embed(&face).context("embedding frame")?);
        println!(
            "  frame {}/{}: mean luma {:.1}{}",
            index + 1,
            args.frames,
            stats.mean,
            if found {
                ", face detected"
            } else {
                ", no face (whole frame)"
            }
        );
    }
    if dark == args.frames {
        println!(
            "Warning: every frame was near-black; the IR emitter is likely off, so this\n\
             enrollment will not match a real face. Storing it anyway for plumbing tests."
        );
    } else if verifier.has_detector() && faceless == args.frames {
        println!("Warning: no face was detected in any frame; the enrollment uses whole frames.");
    }

    let enrollment = Enrollment::new(embeddings).context("building enrollment")?;
    let path = store::save(&store_dir, &user, &enrollment).context("saving enrollment")?;
    println!(
        "Enrolled {user}: {} embeddings (dim {}) saved to {}",
        enrollment.embeddings().len(),
        enrollment.dim(),
        path.display()
    );
    Ok(())
}

/// Lists the users that have an enrollment in the store directory.
fn list(args: ListArgs) -> Result<()> {
    let dir = args.store_dir.unwrap_or_else(default_store_dir);
    let users = store::list(&dir).with_context(|| format!("listing {}", dir.display()))?;
    if users.is_empty() {
        println!("No enrollments in {}.", dir.display());
        return Ok(());
    }
    println!("Enrolled users in {}:", dir.display());
    for user in users {
        println!("  {user}");
    }
    Ok(())
}

/// Enables, disables, or shows face unlock for a PAM service.
fn pam(args: PamArgs) -> Result<()> {
    match args.action {
        PamAction::Status => {
            for service in pamconf::available() {
                let enabled = pamconf::status(service.name)
                    .with_context(|| format!("checking {}", service.name))?;
                let state = if enabled { "enabled" } else { "disabled" };
                println!("  {:<14} {state:<8}  {}", service.name, service.label);
            }
        }
        PamAction::Enable(service) => {
            let changed = pamconf::enable(&service.service)
                .with_context(|| format!("enabling face unlock for {}", service.service))?;
            if changed {
                println!(
                    "Face unlock enabled for {}. Keep a root shell open and test before relying on it.",
                    service.service
                );
            } else {
                println!("Face unlock was already enabled for {}.", service.service);
            }
        }
        PamAction::Disable(service) => {
            let changed = pamconf::disable(&service.service)
                .with_context(|| format!("disabling face unlock for {}", service.service))?;
            if changed {
                println!("Face unlock disabled for {}.", service.service);
            } else {
                println!("Face unlock was not enabled for {}.", service.service);
            }
        }
    }
    Ok(())
}

/// Removes a user's enrollment from the store directory.
fn remove(args: RemoveArgs) -> Result<()> {
    let user = resolve_user(args.user)?;
    let dir = args.store_dir.unwrap_or_else(default_store_dir);
    let removed = store::remove(&dir, &user).with_context(|| format!("removing {user}"))?;
    if removed {
        println!("Removed enrollment for {user}.");
    } else {
        println!("No enrollment for {user} in {}.", dir.display());
    }
    Ok(())
}

/// Resolves the target user, honoring an explicit name or falling back to `$USER`.
fn resolve_user(user: Option<String>) -> Result<String> {
    if let Some(name) = user {
        return Ok(name);
    }
    std::env::var("USER").context("no --user given and $USER is unset")
}

/// Default directory for per-user enrollments when none is given.
fn default_store_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".local/share/xinchao/enroll");
    }
    PathBuf::from("/tmp/xinchao/enroll")
}

/// Shows the effective configuration and reports its file permissions.
fn show_config(args: ConfigArgs) -> Result<()> {
    let path = args
        .path
        .unwrap_or_else(|| PathBuf::from(config::DEFAULT_PATH));
    if let Some(model) = args.set_model {
        config::set_model(&path, model)
            .with_context(|| format!("setting model in {}", path.display()))?;
        println!(
            "Recognition model set to {model:?} in {}.\nRe-enroll each user: embeddings differ between models.",
            path.display()
        );
        return Ok(());
    }
    let config = if path.exists() {
        config::load(&path).with_context(|| format!("loading {}", path.display()))?
    } else {
        println!(
            "No config at {}; showing built-in defaults.\n",
            path.display()
        );
        Config::default()
    };
    print_config(&config);
    if path.exists() {
        match config::check_permissions(&path) {
            Ok(()) => println!("\nPermissions: ok (root-owned, not world-writable)"),
            Err(e) => println!("\nPermissions: WARNING: {e}"),
        }
    }
    Ok(())
}

/// Prints the effective configuration as an annotated TOML-like summary.
fn print_config(config: &Config) {
    let device = config
        .camera
        .device
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(auto-detect)".to_string());
    let capture = config
        .debug
        .capture_path
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(disabled)".to_string());
    println!("[camera]");
    println!("  device        = {device}");
    println!("  frame_width   = {}", config.camera.frame_width);
    println!("  frame_height  = {}", config.camera.frame_height);
    println!("  warmup_frames = {}", config.camera.warmup_frames);
    println!("[recognition]");
    println!("  backend          = {:?}", config.recognition.backend);
    println!("  model            = {:?}", config.recognition.model);
    println!(
        "  detector_path    = {}",
        config.recognition.detector_path.display()
    );
    println!(
        "  model_path       = {}",
        config.recognition.model_path.display()
    );
    println!("  threshold        = {}", config.recognition.threshold);
    println!(
        "  required_matches = {}",
        config.recognition.required_matches
    );
    println!("[auth]");
    println!("  timeout_secs = {}", config.auth.timeout_secs);
    println!("  detect_faces = {}", config.auth.detect_faces);
    println!("[debug]");
    println!("  capture_path = {capture}");
}

/// Probes the camera's extension-unit controls and optionally activates the IR LED.
fn enable_ir(args: EnableIrArgs) -> Result<()> {
    if args.boot {
        return replay_saved_activation();
    }
    let capture = resolve_ir_node(args.device)?;
    emitter::ensure_capture_node(&capture).context("checking IR capture node")?;
    let control = emitter::control_node()
        .context("locating UVC control node")?
        .context("no node exposes UVC extension-unit controls")?;

    println!("IR capture node : {}", capture.display());
    println!("Control node    : {}", control.display());
    println!("\nExtension-unit controls on {}:", control.display());
    let controls = emitter::probe(&control).context("probing extension-unit controls")?;
    for item in &controls {
        let snap = &item.snapshot;
        println!(
            "  unit {:>2} selector {:>2} len {:>3}: cur=[{}] def=[{}] min=[{}] max=[{}]",
            item.unit,
            item.selector,
            snap.len,
            hex(&snap.cur),
            hex(&snap.def),
            hex(&snap.min),
            hex(&snap.max),
        );
    }

    if !args.apply {
        println!("\nRead-only probe. Re-run with --apply to attempt activation.");
        return Ok(());
    }

    println!("\nAttempting activation (writing vendor control payloads) ...");
    match emitter::enable(&control, &capture).context("activating IR emitter")? {
        Some(found) => {
            println!(
                "Emitter lit: unit {} selector {} payload [{}] (mean luma {:.1}).",
                found.unit,
                found.selector,
                hex(&found.payload),
                found.mean,
            );
            persist_activation(&control, &found)?;
            println!(
                "Saved activation to {}; it will be re-armed at boot.",
                config::DEFAULT_PATH
            );
        }
        None => println!(
            "No tried payload lit the emitter. This camera likely needs a vendor\n\
             command sequence (e.g. the Realtek face-auth protocol) beyond a single\n\
             control write. Inspect the values above to investigate further."
        ),
    }
    Ok(())
}

/// Persists a discovered activation to the system config for boot replay.
fn persist_activation(control: &Path, found: &emitter::Activation) -> Result<()> {
    let emitter = config::Emitter {
        control_node: control.to_path_buf(),
        payload: found.payload.clone(),
        selector: found.selector,
        unit: found.unit,
    };
    config::set_emitter(Path::new(config::DEFAULT_PATH), &emitter)
        .context("persisting IR activation to config")
}

/// Replays the saved activation payload to re-arm the emitter, or reports that
/// none has been saved yet. Run by the boot/resume service before any face auth.
fn replay_saved_activation() -> Result<()> {
    let config = config::load_secure(Path::new(config::DEFAULT_PATH))
        .context("loading config to replay IR activation")?;
    match config.emitter {
        Some(emitter) => {
            emitter::apply(
                &emitter.control_node,
                emitter.unit,
                emitter.selector,
                &emitter.payload,
            )
            .context("replaying saved IR activation")?;
            println!(
                "IR emitter armed: wrote [{}] to unit {} selector {} on {}.",
                hex(&emitter.payload),
                emitter.unit,
                emitter.selector,
                emitter.control_node.display(),
            );
        }
        None => println!(
            "No saved IR activation in {}. Run `sudo xinchao enable-ir --apply` \
             once to discover and persist it.",
            config::DEFAULT_PATH
        ),
    }
    Ok(())
}

/// Resolves the IR node to operate on, honoring an explicit `--device` override.
fn resolve_ir_node(device: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = device {
        return Ok(path);
    }
    let devices = camera::enumerate().context("enumerating V4L2 devices")?;
    let ir = camera::detect_ir(&devices).context("no IR node detected; pass --device")?;
    Ok(devices[ir].path.clone())
}

/// Formats bytes as space-separated two-digit hex for diagnostic output.
fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn list_of_empty_store_succeeds() {
        let dir = std::env::temp_dir().join("xinchao-cli-empty-store");
        let _ = std::fs::remove_dir_all(&dir);
        let args = ListArgs {
            store_dir: Some(dir),
        };
        assert!(list(args).is_ok());
    }
}
