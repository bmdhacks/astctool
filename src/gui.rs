// The whole UI: pick a folder or zip, pick block size / quality, watch the bar.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use eframe::egui;

use astctool::pipeline::{self, PipelineOptions, Progress, Report};
use astctool::{report, DEFAULT_FORMAT, DEFAULT_QUALITY, FORMATS};

const QUALITY_PRESETS: [(&str, i32); 4] = [
    ("Fast (10)", 10),
    ("Balanced (50)", 50),
    ("Thorough (98)", DEFAULT_QUALITY),
    ("Exhaustive (100)", 100),
];

enum Msg {
    Progress(Progress),
    Finished(Result<Report, String>),
}

#[derive(PartialEq)]
enum Status {
    Idle,
    Running,
    Done,
    Failed,
}

struct App {
    input: Option<PathBuf>,
    format_idx: usize,
    quality_idx: usize,
    jobs: usize,
    status: Status,
    phase: String,
    done: usize,
    total: usize,
    failed: usize,
    eta: Option<Duration>,
    log: Vec<String>,
    report: Option<Report>,
    error: Option<String>,
    rx: Option<Receiver<Msg>>,
}

impl App {
    fn new() -> Self {
        let format_idx = FORMATS.iter().position(|f| *f == DEFAULT_FORMAT).unwrap_or(2);
        let quality_idx = QUALITY_PRESETS
            .iter()
            .position(|(_, q)| *q == DEFAULT_QUALITY)
            .unwrap_or(2);
        Self {
            input: None,
            format_idx,
            quality_idx,
            jobs: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            status: Status::Idle,
            phase: String::new(),
            done: 0,
            total: 0,
            failed: 0,
            eta: None,
            log: Vec::new(),
            report: None,
            error: None,
            rx: None,
        }
    }

    fn pick(&mut self, folder: bool) {
        let dialog = rfd::FileDialog::new();
        let picked = if folder {
            dialog.pick_folder()
        } else {
            dialog.add_filter("texture pack zip", &["zip"]).pick_file()
        };
        if let Some(p) = picked {
            self.input = Some(p);
        }
    }

    fn start(&mut self) {
        let Some(input) = self.input.clone() else {
            self.error = Some("pick a folder or .zip first".into());
            return;
        };
        let opts = PipelineOptions {
            input,
            output: None,
            format: FORMATS[self.format_idx].to_string(),
            quality: QUALITY_PRESETS[self.quality_idx].1,
            jobs: self.jobs,
        };
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.status = Status::Running;
        self.phase = "starting".into();
        self.done = 0;
        self.total = 0;
        self.failed = 0;
        self.eta = None;
        self.log.clear();
        self.report = None;
        self.error = None;

        std::thread::spawn(move || {
            let tx_progress = tx.clone();
            let mut progress = move |p: Progress| {
                let _ = tx_progress.send(Msg::Progress(p));
            };
            let result = pipeline::run(&opts, &mut progress);
            let result = match result {
                Ok(rep) => {
                    if let Err(e) =
                        report::write_report(&rep, std::path::Path::new(&rep.output))
                    {
                        log::warn!("could not write report: {e}");
                    }
                    Ok(rep)
                }
                Err(e) => Err(format!("{e:#}")),
            };
            let _ = tx.send(Msg::Finished(result));
        });
    }

    fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        loop {
            match rx.try_recv() {
                Ok(Msg::Progress(Progress::Phase(p))) => self.phase = p,
                Ok(Msg::Progress(Progress::Discovered {
                    families,
                    unsupported,
                })) => {
                    self.total = families;
                    self.log
                        .push(format!("found {families} textures ({unsupported} unsupported)"));
                }
                Ok(Msg::Progress(Progress::Encode {
                    done,
                    total,
                    failed,
                    eta,
                    ..
                })) => {
                    self.done = done;
                    self.total = total;
                    self.failed = failed;
                    self.eta = eta;
                }
                Ok(Msg::Progress(Progress::Validate { done, total, failed })) => {
                    self.done = done;
                    self.total = total;
                    self.failed = failed;
                }
                Ok(Msg::Progress(Progress::Log(line))) => self.log.push(line),
                Ok(Msg::Finished(Ok(rep))) => {
                    self.done = rep.succeeded;
                    self.total = rep.total_textures;
                    self.failed = rep.failed_count;
                    self.report = Some(rep);
                    self.status = Status::Done;
                    break;
                }
                Ok(Msg::Finished(Err(e))) => {
                    self.error = Some(e);
                    self.status = Status::Failed;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if self.status == Status::Running {
                        self.error = Some("worker stopped unexpectedly".into());
                        self.status = Status::Failed;
                    }
                    break;
                }
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll();
        if self.status == Status::Running {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }

        // Drag and drop a folder or zip onto the window.
        let dropped: Vec<PathBuf> = ui.ctx().input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if let Some(p) = dropped.into_iter().next() {
            self.input = Some(p);
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(4.0);
            ui.heading("ASTC Texture Pack Builder");
            ui.label("Point it at a PCSX2 texture folder (or a .zip of one).");
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if ui.button("Choose folder…").clicked() {
                    self.pick(true);
                }
                if ui.button("Choose .zip…").clicked() {
                    self.pick(false);
                }
                match &self.input {
                    Some(p) => {
                        ui.monospace(shorten(p));
                    }
                    None => {
                        ui.weak("nothing selected");
                    }
                }
            });

            ui.add_space(8.0);
            egui::Grid::new("opts").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Block size");
                egui::ComboBox::from_id_salt("format")
                    .selected_text(FORMATS[self.format_idx])
                    .show_ui(ui, |ui| {
                        for (i, f) in FORMATS.iter().enumerate() {
                            ui.selectable_value(&mut self.format_idx, i, *f);
                        }
                    });
                ui.end_row();

                ui.label("Quality");
                egui::ComboBox::from_id_salt("quality")
                    .selected_text(QUALITY_PRESETS[self.quality_idx].0)
                    .show_ui(ui, |ui| {
                        for (i, (name, _)) in QUALITY_PRESETS.iter().enumerate() {
                            ui.selectable_value(&mut self.quality_idx, i, *name);
                        }
                    });
                ui.end_row();

                ui.label("Parallel jobs");
                ui.add(egui::Slider::new(&mut self.jobs, 1..=64));
                ui.end_row();
            });

            ui.add_space(8.0);
            let can_build = self.status != Status::Running && self.input.is_some();
            if ui
                .add_enabled(can_build, egui::Button::new("Build pack"))
                .clicked()
            {
                self.start();
            }

            ui.add_space(12.0);
            self.status_ui(ui);
        });
    }
}

impl App {
    fn status_ui(&mut self, ui: &mut egui::Ui) {
        match self.status {
            Status::Idle => {}
            Status::Running => {
                ui.label(format!("{}…", self.phase));
                let frac = if self.total > 0 {
                    (self.done as f32 / self.total as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                ui.add(egui::ProgressBar::new(frac).show_percentage().animate(true));
                ui.horizontal(|ui| {
                    ui.label(format!("{} / {}", self.done, self.total));
                    if self.failed > 0 {
                        ui.colored_label(egui::Color32::from_rgb(220, 120, 0), format!("{} failed", self.failed));
                    }
                    ui.weak(format!("ETA {}", fmt_eta(self.eta)));
                });
            }
            Status::Done => {
                if let Some(rep) = &self.report {
                    ui.colored_label(
                        egui::Color32::from_rgb(40, 160, 60),
                        format!(
                            "Pack built: {}/{} textures, {} failed in {:.1}s",
                            rep.succeeded,
                            rep.total_textures,
                            rep.failed_count,
                            rep.elapsed_ms as f64 / 1000.0
                        ),
                    );
                    ui.monospace(shorten(std::path::Path::new(&rep.output)));
                    ui.horizontal(|ui| {
                        if ui.button("Open output folder").clicked() {
                            open_path(std::path::Path::new(&rep.output));
                        }
                    });
                    if !rep.failures.is_empty() {
                        egui::CollapsingHeader::new(format!("{} failures", rep.failures.len()))
                            .default_open(false)
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                                    for f in &rep.failures {
                                        ui.monospace(format!("[{}] {}: {}", f.stage, f.path, f.error));
                                    }
                                });
                            });
                    }
                }
            }
            Status::Failed => {
                if let Some(e) = &self.error {
                    ui.colored_label(egui::Color32::from_rgb(200, 60, 60), e);
                }
            }
        }

        if !self.log.is_empty() {
            ui.add_space(8.0);
            egui::CollapsingHeader::new("Log")
                .default_open(false)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                        for l in &self.log {
                            ui.monospace(l);
                        }
                    });
                });
        }
    }
}

fn shorten(p: &std::path::Path) -> String {
    let s = p.display().to_string();
    if s.chars().count() > 70 {
        let tail: String = s.chars().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect();
        format!("…{tail}")
    } else {
        s
    }
}

fn fmt_eta(eta: Option<Duration>) -> String {
    match eta {
        None => "--".into(),
        Some(d) => {
            let s = d.as_secs();
            if s >= 3600 {
                format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
            } else if s >= 60 {
                format!("{}m{:02}s", s / 60, s % 60)
            } else {
                format!("{s}s")
            }
        }
    }
}

fn open_path(path: &std::path::Path) {
    let dir = path.parent().unwrap_or(path);
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";
    let _ = std::process::Command::new(cmd).arg(dir).spawn();
}

pub fn run() -> i32 {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 660.0])
            .with_min_inner_size([480.0, 520.0])
            .with_drag_and_drop(true)
            .with_title("ASTC Texture Pack Builder"),
        ..Default::default()
    };
    match eframe::run_native(
        "astctool",
        options,
        Box::new(|_cc| Ok(Box::new(App::new()))),
    ) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("gui error: {e}");
            1
        }
    }
}
