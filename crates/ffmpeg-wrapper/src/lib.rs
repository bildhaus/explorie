//! Minimal FFmpeg command builder used by explorie.
//!
//! This is intentionally lightweight so we can compose commands without
//! requiring FFmpeg to be present at test time. Callers can inspect the
//! generated arguments or hand the `Command` off to a worker process.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Representation of an FFmpeg call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegCommand {
    pub binary: PathBuf,
    pub args: Vec<String>,
}

impl FfmpegCommand {
    /// Convert into a `std::process::Command`. Does not spawn the process.
    pub fn to_process(&self) -> Command {
        let mut cmd = Command::new(&self.binary);
        cmd.args(&self.args);
        cmd
    }
}

/// Builder for assembling common FFmpeg arguments without spawning processes.
#[derive(Debug, Clone)]
pub struct FfmpegTask {
    pub input: PathBuf,
    pub output: PathBuf,
    pub overwrite: bool,
    pub log_level: Option<String>,
    pub video_filters: Vec<String>,
    pub audio_filters: Vec<String>,
    pub copy_audio: bool,
    pub copy_video: bool,
    pub extra_args: Vec<String>,
    pub binary: Option<PathBuf>,
}

impl FfmpegTask {
    pub fn new(input: impl AsRef<Path>, output: impl AsRef<Path>) -> Self {
        Self {
            input: input.as_ref().to_path_buf(),
            output: output.as_ref().to_path_buf(),
            overwrite: true,
            log_level: None,
            video_filters: Vec::new(),
            audio_filters: Vec::new(),
            copy_audio: false,
            copy_video: false,
            extra_args: Vec::new(),
            binary: None,
        }
    }

    pub fn add_video_filter(mut self, filter: impl Into<String>) -> Self {
        self.video_filters.push(filter.into());
        self
    }

    pub fn add_audio_filter(mut self, filter: impl Into<String>) -> Self {
        self.audio_filters.push(filter.into());
        self
    }

    pub fn copy_video(mut self, copy: bool) -> Self {
        self.copy_video = copy;
        self
    }

    pub fn copy_audio(mut self, copy: bool) -> Self {
        self.copy_audio = copy;
        self
    }

    pub fn add_arg(mut self, arg: impl Into<String>) -> Self {
        self.extra_args.push(arg.into());
        self
    }

    /// Build the command representation.
    pub fn build(self) -> FfmpegCommand {
        let mut args = Vec::new();
        if self.overwrite {
            args.push("-y".into());
        }
        if let Some(level) = self.log_level {
            args.push("-loglevel".into());
            args.push(level);
        }
        args.push("-i".into());
        args.push(self.input.to_string_lossy().to_string());

        if self.copy_video {
            args.push("-c:v".into());
            args.push("copy".into());
        }
        if !self.video_filters.is_empty() {
            args.push("-vf".into());
            args.push(self.video_filters.join(","));
        }

        if self.copy_audio {
            args.push("-c:a".into());
            args.push("copy".into());
        }
        if !self.audio_filters.is_empty() {
            args.push("-af".into());
            args.push(self.audio_filters.join(","));
        }

        args.extend(self.extra_args);
        args.push(self.output.to_string_lossy().to_string());

        let binary = self.binary.unwrap_or_else(|| PathBuf::from("ffmpeg"));

        FfmpegCommand { binary, args }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after unix epoch")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "explorie-ffmpeg-wrapper-{name}-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("test temp dir should be created");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn ffmpeg_binary_for_integration_test() -> Option<PathBuf> {
        let binary = std::env::var_os("EXPLORIE_FFMPEG_TEST_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("ffmpeg"));
        let output = Command::new(&binary).arg("-version").output().ok()?;
        output.status.success().then_some(binary)
    }

    #[test]
    fn builds_expected_arguments() {
        let cmd = FfmpegTask::new("in.mp4", "out.webm")
            .add_video_filter("scale=1280:720")
            .add_audio_filter("volume=0.8")
            .add_arg("-preset:v")
            .add_arg("veryfast")
            .build();

        assert_eq!(cmd.binary, PathBuf::from("ffmpeg"));
        assert_eq!(
            cmd.args,
            vec![
                "-y",
                "-i",
                "in.mp4",
                "-vf",
                "scale=1280:720",
                "-af",
                "volume=0.8",
                "-preset:v",
                "veryfast",
                "out.webm"
            ]
        );
    }

    #[test]
    fn respects_copy_flags() {
        let cmd = FfmpegTask::new("clip.mov", "out.mov")
            .add_video_filter("scale=640:-2")
            .add_audio_filter("volume=1.2")
            .add_arg("-progress")
            .add_arg("pipe:1")
            .build();
        // default copy flags are false; ensure filters are present
        assert!(cmd.args.contains(&"-vf".to_string()));
        assert!(cmd.args.contains(&"-af".to_string()));

        let copied = FfmpegTask::new("clip.mov", "out.mov")
            .copy_video(true)
            .copy_audio(true)
            .add_arg("-movflags")
            .add_arg("+faststart")
            .build();
        assert!(copied.args.windows(2).any(|w| w == ["-c:v", "copy"]));
        assert!(copied.args.windows(2).any(|w| w == ["-c:a", "copy"]));
    }

    #[test]
    fn allows_custom_binary() {
        let cmd = FfmpegTask {
            binary: Some(PathBuf::from("/opt/bin/ffmpeg")),
            ..FfmpegTask::new("a.mkv", "b.mkv")
        }
        .build();
        assert_eq!(cmd.binary, PathBuf::from("/opt/bin/ffmpeg"));
    }

    #[test]
    fn extracts_thumbnail_when_ffmpeg_is_available() {
        let Some(binary) = ffmpeg_binary_for_integration_test() else {
            eprintln!("skipping ffmpeg integration test: ffmpeg binary not available");
            return;
        };

        let dir = TestDir::new("thumbnail");
        let input = dir.path().join("fixture.mpg");
        let output = dir.path().join("thumbnail.png");

        let fixture = Command::new(&binary)
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x64:rate=25",
                "-t",
                "1",
                "-c:v",
                "mpeg1video",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&input)
            .output()
            .expect("ffmpeg should run to create a video fixture");

        assert!(
            fixture.status.success(),
            "ffmpeg fixture creation failed: {}",
            String::from_utf8_lossy(&fixture.stderr)
        );

        let command = FfmpegTask {
            binary: Some(binary),
            ..FfmpegTask::new(&input, &output)
        }
        .add_arg("-frames:v")
        .add_arg("1")
        .build();

        let thumbnail = command
            .to_process()
            .output()
            .expect("ffmpeg should run to extract a thumbnail");

        assert!(
            thumbnail.status.success(),
            "ffmpeg thumbnail extraction failed: {}",
            String::from_utf8_lossy(&thumbnail.stderr)
        );

        let metadata = fs::metadata(output).expect("thumbnail output should exist");
        assert!(metadata.len() > 0, "thumbnail output should not be empty");
    }
}
