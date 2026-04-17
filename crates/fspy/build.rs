use std::{
    env::{self, current_dir},
    fmt::Write as _,
    fs,
    io::{Cursor, Read},
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};

fn download(url: &str) -> anyhow::Result<Vec<u8>> {
    let curl = Command::new("curl")
        .args([
            "-f", // fail on HTTP errors
            "-L", // follow redirects
            url,
        ])
        .stdout(Stdio::piped())
        .spawn()?;
    let output = curl.wait_with_output()?;
    if !output.status.success() {
        bail!("curl exited with status {} trying to download {}", output.status, url);
    }
    Ok(output.stdout)
}

fn unpack_tar_gz(tarball: impl Read, path: &str) -> anyhow::Result<Vec<u8>> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let tar = GzDecoder::new(tarball);
    let mut archive = Archive::new(tar);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path_bytes().as_ref() == path.as_bytes() {
            let mut data = Vec::<u8>::with_capacity(entry.size().try_into().unwrap());
            entry.read_to_end(&mut data)?;
            return Ok(data);
        }
    }
    bail!("Path {path} not found in tar gz")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        write!(&mut s, "{b:02x}").unwrap();
    }
    s
}

/// `(url, path_in_targz, expected_sha256_of_tarball)`
///
/// The SHA-256 verifies the tarball (the file served by the GitHub release
/// asset URL). Neither upstream currently publishes a `*.sha256` file, so
/// these values are the hash of the asset at the time it was pinned.
///
/// To verify or refresh after a release bump:
///   `curl -sL <url> | shasum -a 256`
type BinaryDownload = (&'static str, &'static str, &'static str);

const MACOS_BINARY_DOWNLOADS: &[(&str, &[BinaryDownload])] = &[
    (
        "aarch64",
        &[
            // https://github.com/branchseer/oils-for-unix-build/releases/tag/oils-for-unix-0.37.0
            (
                "https://github.com/branchseer/oils-for-unix-build/releases/download/oils-for-unix-0.37.0/oils-for-unix-0.37.0-darwin-arm64.tar.gz",
                "oils-for-unix",
                "3a35f7ae2be85fcd32392cd8171522f5822f20a69125c5e9d8d68b2f5c857098",
            ),
            // https://github.com/uutils/coreutils/releases/tag/0.4.0
            (
                "https://github.com/uutils/coreutils/releases/download/0.4.0/coreutils-0.4.0-aarch64-apple-darwin.tar.gz",
                "coreutils-0.4.0-aarch64-apple-darwin/coreutils",
                "a148b660eeaf409af7a4406903f93d0e6713a5eb9adcaf71a1d732f1e3cc3522",
            ),
        ],
    ),
    (
        "x86_64",
        &[
            // https://github.com/branchseer/oils-for-unix-build/releases/tag/oils-for-unix-0.37.0
            (
                "https://github.com/branchseer/oils-for-unix-build/releases/download/oils-for-unix-0.37.0/oils-for-unix-0.37.0-darwin-x86_64.tar.gz",
                "oils-for-unix",
                "aa12258d1bd553020144ad61fdac18e7dfbe3fc3965da32ee458840153169151",
            ),
            // https://github.com/uutils/coreutils/releases/tag/0.4.0
            (
                "https://github.com/uutils/coreutils/releases/download/0.4.0/coreutils-0.4.0-x86_64-apple-darwin.tar.gz",
                "coreutils-0.4.0-x86_64-apple-darwin/coreutils",
                "6e4be8429efe86c9a60247ae7a930221ed11770a975fb4b6fd09ff8d39b9a15c",
            ),
        ],
    ),
];

fn fetch_macos_binaries() -> anyhow::Result<()> {
    if env::var("CARGO_CFG_TARGET_OS").unwrap() != "macos" {
        return Ok(());
    }

    let out_dir = current_dir().unwrap().join(Path::new(&std::env::var_os("OUT_DIR").unwrap()));

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let downloads = MACOS_BINARY_DOWNLOADS
        .iter()
        .find(|(arch, _)| *arch == target_arch)
        .context(format!("Unsupported macOS arch: {target_arch}"))?
        .1;

    for (url, path_in_targz, expected_sha256) in downloads.iter().copied() {
        let filename = path_in_targz.split('/').next_back().unwrap();
        let download_path = out_dir.join(filename);

        let data = if let Ok(cached) = fs::read(&download_path) {
            cached
        } else {
            let tarball = download(url).context(format!("Failed to download {url}"))?;
            let actual_sha256 = sha256_hex(&tarball);
            assert_eq!(
                actual_sha256, expected_sha256,
                "sha256 of {url} does not match — update expected value in MACOS_BINARY_DOWNLOADS",
            );
            unpack_tar_gz(Cursor::new(tarball), path_in_targz)
                .context(format!("Failed to extract {path_in_targz} from {url}"))?
        };
        embedded_artifact_build::write_artifact(&out_dir, filename, &data)
            .context(format!("Writing artifact {filename} to {}", out_dir.display()))?;
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    println!("cargo:rerun-if-changed=build.rs");
    fetch_macos_binaries().context("Failed to fetch macOS binaries")?;
    Ok(())
}
