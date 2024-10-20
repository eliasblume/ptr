use anyhow::{anyhow, bail, Result};
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, ACCEPT, USER_AGENT};
use serde::Deserialize;
use std::fs::{self, File};
use std::io::{self, Write};
use std::{env, mem};
use std::path::{Path, PathBuf};
use std::process::Command;
use zip::ZipArchive;

use crate::polling;
use crate::PLUGIN_PATH;

#[derive(Deserialize)]
struct ApiResponse {
	tag_name: String,
	assets: Box<[Assets]>,
}

#[derive(Deserialize)]
struct Assets {
	name: String,
	browser_download_url: String,
}
#[macro_export]
macro_rules! gh_dl {
	($root_name:expr, $repo:expr, $version:expr, $arch:expr) => {
		$crate::util::gh_dl($root_name, $repo, $version, $arch, None)
	};
	($root_name:expr, $repo:expr, $version:expr, $arch:expr, $current_version:expr) => {
		$crate::util::gh_dl($root_name, $repo, $version, $arch, Some($current_version))
	};
}

/// Download a file from a GitHub repository.
///
/// # Arguments
///
/// * `repo` - The repository to download from.
/// * `version` - The tagged version of the repository to download.
/// * `arch` - The architecture of the system.
/// * `current_version` - The current version of the repository that is installed.
///
/// # Returns
/// The version of the repository that was downloaded.
pub fn gh_dl(
	root_name: &str,
	repo: &str,
	version: Option<String>,
	arch: &str,
	current_version: Option<String>,
) -> Result<String> {
	let url = if let Some(version) = version {
		format!(
			"https://api.github.com/repos/{}/releases/tags/{}",
			repo, version
		)
	} else {
		format!("https://api.github.com/repos/{}/releases/latest", repo)
	};
	let mut headers = HeaderMap::new();
	headers.insert(USER_AGENT, "reqwest".parse().unwrap());
	headers.insert(ACCEPT, "application/vnd.github+json".parse().unwrap());
	headers.insert("X-GitHub-Api-Version", "2022-11-28".parse().unwrap());
	let res = Client::new().get(&url).headers(headers).send()?;
	if !res.status().is_success() {
		bail!(
			"Failed to fetch the latest release: {}",
			res.status().canonical_reason().unwrap_or("Unknown"),
		);
	}
	let res = res.json::<ApiResponse>()?;
	let tag = res.tag_name;
	if let Some(current_version) = current_version {
		if tag == current_version {
			return Ok(current_version);
		}
	}

	let asset = &res
		.assets
		.iter()
		.find(|a|
			(a.name.contains(arch) || a.name.contains(&arch.to_uppercase()))
				&& a.name.ends_with(".zip")
		)
		.ok_or(anyhow!("No asset found than contains '{}'", arch))?;
	let (url, name) = (&asset.browser_download_url, &asset.name);
	let res = Client::new().get(url).send()?;

	let file_path = PLUGIN_PATH.join(name);
	let mut file = File::create(&file_path)?;
	file.write_all(&res.bytes()?)?;

	extract_zip(&file_path, &PLUGIN_PATH, root_name)?;
	fs::remove_file(&file_path)?;

	Ok(tag)
}

fn extract_zip(zip_path: &Path, output_dir: &Path, root_name: &str) -> Result<()> {
	let file = File::open(zip_path)?;
	let mut archive = ZipArchive::new(file)?;

	// extract all files and keep the directory structure
	for i in 0..archive.len() {
		let mut file = archive.by_index(i)?;
		let out_path = Path::new(output_dir).join(file.name());

		if (file.name()).ends_with('/') {
			fs::create_dir_all(&out_path)?;
		} else {
			if let Some(p) = out_path.parent() {
				if !p.exists() {
					fs::create_dir_all(p)?;
				}
			}
			let mut out_file = File::create(&out_path)?;
			polling::copy(&mut file, &mut out_file)?;
		}
	}

	let extracted_root = output_dir.join(archive.by_index(0)?.name().split('/').next().unwrap());
	let root_path = output_dir.join(root_name);
	if extracted_root != root_path {
		// extracting to a different directory means we're not done polling for file access during extracting.
		if root_path.exists() {
			polling::remove_dir_all(&root_path)?;
		}
		fs::rename(extracted_root, &root_path)?;
	}

	Ok(())
}

fn run_as_admin(program: &str, args: &str) -> Result<()> {
	use windows::core::{w, HSTRING, PCWSTR};
	use windows::Win32::Foundation::CloseHandle;
	use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
	use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};

	let mut sei: SHELLEXECUTEINFOW = unsafe { mem::zeroed() };
	sei.cbSize = mem::size_of::<SHELLEXECUTEINFOW>() as u32;
	sei.fMask = SEE_MASK_NOCLOSEPROCESS;
	sei.lpVerb = w!("runas");
	let h_file = HSTRING::from(program);
	sei.lpFile = PCWSTR(h_file.as_ptr());
	sei.nShow = 0; // SW_HIDE
	let h_args = HSTRING::from(args);
	sei.lpParameters = PCWSTR(h_args.as_ptr());

	unsafe {
		ShellExecuteExW(&mut sei)?;
		let process = sei.hProcess;
		_ = WaitForSingleObject(process, INFINITE);
		let mut exit_code = 0;
		GetExitCodeProcess(process, &mut exit_code)?;
		CloseHandle(process)?;

		if exit_code == 0 {
			Ok(())
		} else {
			Err(io::Error::from_raw_os_error(exit_code as i32).into())
		}
	}
}

pub fn kill_ptr() -> Result<()> {
	run_as_admin("taskkill.exe", "/F /FI \"IMAGENAME eq PowerToys*\"")?;
	Ok(())
}

pub fn start_ptr() -> Result<()> {
	let powertoys_path = get_powertoys_path()?;
	let c = Command::new(powertoys_path).spawn()?;
	mem::forget(c);
	Ok(())
}

fn get_powertoys_path() -> Result<String> {
	let possible_paths = [
		PathBuf::from(r"C:\Program Files\PowerToys\PowerToys.exe"),
		env::var("LOCALAPPDATA")
			.map(|app_data| PathBuf::from(app_data).join(r"PowerToys\PowerToys.exe"))
			.unwrap_or_else(|_| PathBuf::new()),
	];
	for path in &possible_paths {
		if path.exists() {
			return Ok(path.to_path_buf().to_string_lossy().to_string());
		}
	}
	Err(anyhow!("PowerToys executable not found in any of the expected locations"))
}

#[macro_export]
macro_rules! tabwriter {
    ($fmt:expr, $($arg:tt)*) => {{
        use std::io::Write;
        use colored::Colorize;
        let mut tw = tabwriter::TabWriter::new(vec![]);
        write!(&mut tw, $fmt, $($arg)*).expect("Failed to write to TabWriter");
        tw.flush().expect("Failed to flush TabWriter");
        println!("{}", String::from_utf8(tw.into_inner().unwrap()).unwrap());
    }};
}

#[macro_export]
macro_rules! print_message {
    ($symbol:expr, $color:ident, $msg:expr) => {
        $crate::tabwriter!("{} {}", $symbol.$color().bold(), $msg)
    };
    ($symbol:expr, $color:ident, $fmt:expr, $($arg:tt)*) => {
        $crate::tabwriter!("{} {}", $symbol.$color().bold(), format!($fmt, $($arg)*))
    };
}

/// print message for adding an item.
#[macro_export]
macro_rules! add {
    ($($arg:tt)*) => {
        $crate::print_message!("+", bright_green, $($arg)*)
    };
}

/// print message for item that is up to date.
#[macro_export]
macro_rules! up_to_date {
    ($($arg:tt)*) => {
        $crate::print_message!("=", bright_blue, $($arg)*)
    };
}

/// print message for removing an item.
#[macro_export]
macro_rules! remove {
    ($($arg:tt)*) => {
        $crate::print_message!("-", bright_red, $($arg)*)
    };
}

/// Print an error message to stderr.
#[macro_export]
macro_rules! error {
    ($msg:expr) => {{
        use colored::Colorize;
        eprintln!("{} {}", "error:".bright_red().bold(), $msg)
    }};
    ($fmt:expr, $($arg:tt)*) => {{
        use colored::Colorize;
        eprintln!("{} {}", "error:".bright_red().bold(), format!($fmt, $($arg)*))
    }};
}

/// Print a error message to stderr and exit with code 0.
#[macro_export]
macro_rules! exit {
    ($msg:expr) => {{
        $crate::error!($msg);
        std::process::exit(0);
    }};
    ($fmt:expr, $($arg:tt)*) => {{
        $crate::error!($fmt, $($arg)*);
        std::process::exit(0);
    }};
}
