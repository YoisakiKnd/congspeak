//! Compare resident memory with and without eight idle audio workers on macOS/Linux.
//! The workers never open an input or output device.
use congspeak_audio::runtime::AudioHandle;
use congspeak_core::AudioSettings;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workers: usize = std::env::args().nth(1).as_deref().unwrap_or("0").parse()?;
    let workers: Vec<_> = (0..workers)
        .map(|_| AudioHandle::spawn(AudioSettings::default()))
        .collect();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()?;
    if !output.status.success() {
        return Err("ps could not read process memory".into());
    }
    println!(
        "workers={} rss_kib={}",
        workers.len(),
        String::from_utf8_lossy(&output.stdout).trim()
    );
    Ok(())
}
