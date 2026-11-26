use std::{
    convert::identity,
    fs, io,
    path::{Path, PathBuf},
};

use super::platform;
use crate::build_support::{
    cargo,
    features::{self, Features, feature},
};

pub const SKIA_OUTPUT_DIR: &str = "skia";
const ICUDTL_DAT: &str = "icudtl.dat";
const BINDINGS_RS: &str = "bindings.rs";

/// The libraries to link with.
pub mod lib {
    pub const SKIA: &str = "skia";
    pub const SKIA_BINDINGS: &str = "skia-bindings";
    pub const SK_SHAPER: &str = "skshaper";
    pub const SK_PARAGRAPH: &str = "skparagraph";
    pub const SVG: &str = "svg";
    pub const SKOTTIE: &str = "skottie";
    pub const SKSG: &str = "sksg";
    pub const JSON_READER: &str = "jsonreader";
    pub const SK_RESOURCES: &str = "skresources";
    pub const SK_UNICODE_CORE: &str = "skunicode_core";
    pub const SK_UNICODE_ICU: &str = "skunicode_icu";
}

/// The configuration of the resulting binaries.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BinariesConfiguration {
    /// The features we built with.
    pub features: Features,

    /// The output directory of the libraries we build and we need to inform cargo about.
    pub output_directory: PathBuf,

    /// The TARGET specific link libraries we need to inform cargo about.
    pub link_libraries: Vec<String>,

    /// The static Skia libraries built by ninja that dependent projects need to link with.
    pub ninja_built_libraries: Vec<String>,

    /// Static libraries that are generated in the binding process dependent projects need to link
    /// with.
    pub binding_libraries: Vec<String>,

    /// Files that are generated in the binding process (this is `bindings.rs`).
    pub binding_files: Vec<PathBuf>,

    /// Additional files relative to the output_directory
    /// that are needed to build dependent projects.
    pub additional_files: Vec<PathBuf>,

    /// `true` if the skia libraries are built with debugging information.
    pub skia_debug: bool,
}

impl BinariesConfiguration {
    /// Build a binaries configuration from a set of Skia features.
    pub fn from_features(features: &features::Features, skia_debug: bool) -> Self {
        let target = cargo::target();

        let mut ninja_built_libraries = Vec::new();
        let mut binding_libraries = Vec::new();
        let binding_files = vec!["bindings.rs".into()];
        let mut additional_files = Vec::new();

        if features[feature::TEXTLAYOUT] {
            if target.is_windows() {
                additional_files.push(ICUDTL_DAT.into());
            }
            ninja_built_libraries.push(lib::SK_PARAGRAPH.into());
            ninja_built_libraries.push(lib::SK_SHAPER.into());
            ninja_built_libraries.push(lib::SK_UNICODE_CORE.into());
            ninja_built_libraries.push(lib::SK_UNICODE_ICU.into());
        }
        if features[feature::SVG] {
            ninja_built_libraries.push(lib::SVG.into());
            ninja_built_libraries.push(lib::SK_RESOURCES.into());
        }
        if features[feature::SKOTTIE] {
            ninja_built_libraries.push(lib::SKOTTIE.into());
            ninja_built_libraries.push(lib::SKSG.into());
            ninja_built_libraries.push(lib::JSON_READER.into());
            // skottie depends on skresources, add it if not already added by SVG
            if !features[feature::SVG] {
                ninja_built_libraries.push(lib::SK_RESOURCES.into());
            }
        }

        let link_libraries = platform::link_libraries(features, &target);

        let output_directory = if cfg!(feature = "use-common-compile-directory") {
            let common_out_dir =
                common_output_directory().expect("Failed to find target directory");

            let mut subdir = features.to_key();
            if subdir.is_empty() {
                subdir = "no-features".into();
            }

            common_out_dir
                .join("skia_binaries")
                .join(subdir)
                .join(SKIA_OUTPUT_DIR)
        } else {
            cargo::output_directory().join(SKIA_OUTPUT_DIR)
        };

        //
        // cargo::set_env_var("SKIA_COMPILE_DIR", output_directory.to_str().unwrap());

        ninja_built_libraries.push(lib::SKIA.into());
        binding_libraries.push(lib::SKIA_BINDINGS.into());

        BinariesConfiguration {
            features: features.clone(),
            output_directory,
            link_libraries,
            ninja_built_libraries,
            binding_libraries,
            binding_files,
            additional_files,
            skia_debug,
        }
    }

    pub fn built_libraries(&self, include_bindings: bool) -> impl Iterator<Item = &str> {
        self.ninja_built_libraries
            .iter()
            .chain(if include_bindings {
                self.binding_libraries.iter()
            } else {
                [].iter()
            })
            .map(|x| x.as_str())
    }

    /// Inform cargo that the library files of the given configuration are available and
    /// can be used as dependencies.
    pub fn commit_to_cargo(&self) {
        cargo::add_link_search(self.output_directory.to_str().unwrap());

        let target = cargo::target();

        // On Linux, the order is significant, first the static libraries we built, and then
        // the system libraries.
        cargo::add_static_link_libs(&target, self.built_libraries(true));
        cargo::add_link_libs(&self.link_libraries);
    }

    pub fn copy_emscripten_ninja_archives_for_linking(&self) {
        // Since Skia milestone 148, the wasm GN toolchain emits static archives as
        // `*.wasm.a`.
        for lib in &self.ninja_built_libraries {
            let from = self.output_directory.join(format!("lib{lib}.wasm.a"));
            let to = self.output_directory.join(format!("lib{lib}.a"));
            fs::copy(&from, &to).unwrap_or_else(|e| {
                panic!(
                    "failed to prepare emscripten archive for linking: from '{}' to '{}': {}",
                    from.display(),
                    to.display(),
                    e
                )
            });
        }
    }

    /// Import library and additional files from `from_dir` to the output directory.
    pub fn import(&self, from_dir: &Path, import_bindings: bool) -> io::Result<()> {
        let output_directory = &self.output_directory;
        self.copy_libs_and_additional_files(from_dir, output_directory, import_bindings)
    }

    /// Export library and additional files from the output directory to a `to_dir`.
    #[allow(unused)]
    pub fn export(&self, to_dir: &Path) -> io::Result<()> {
        let output_directory = &self.output_directory;
        self.copy_libs_and_additional_files(output_directory, to_dir, true)
    }

    /// Copy files like bindings.rs and icudtl.dat to OUT_DIR, because
    /// rust-analyzers shits itself when it sees an `include!` outside the project
    ///
    /// So instead of including the file in the compile dir, we just copy it into the OUT_DIR where
    /// rust-analyzer would recognize the file
    pub fn copy_included_files_to_out_dir(&self) {
        let mut files_to_copy = vec![BINDINGS_RS];

        if self.features[feature::TEXTLAYOUT] && cargo::target().is_windows() {
            files_to_copy.push(ICUDTL_DAT);
        }

        let out_dir = cargo::output_directory().join(SKIA_OUTPUT_DIR);
        fs::create_dir_all(&out_dir).expect("Failed to create OUT_DIR");

        for file in files_to_copy {
            let file_path = PathBuf::from(file);
            copy_file(&file_path, &self.output_directory, &out_dir).unwrap();
        }
    }

    fn copy_libs_and_additional_files(
        &self,
        from_dir: &Path,
        to_dir: &Path,
        copy_bindings: bool,
    ) -> io::Result<()> {
        fs::create_dir_all(to_dir)?;

        let target = cargo::target();

        for lib in self.built_libraries(copy_bindings) {
            let filename = &target.library_to_filename(lib);
            copy_file(filename, from_dir, to_dir)?;
        }

        for filename in &self.additional_files {
            copy_file(filename, from_dir, to_dir)?;
        }

        if copy_bindings {
            for filename in &self.binding_files {
                copy_file(filename, from_dir, to_dir)?;
            }
        }

        return Ok(());
    }
}

fn common_output_directory() -> io::Result<PathBuf> {
    // TODO: Verst: provide an override via an environment variable

    // Verst:
    // I wanna reuse the output directory between debug and release builds,
    // this requires hacking around cargo a little bit
    //
    // We basically wanna have a path like project/target/skia_binaries/...
    let cargo_out_dir = cargo::output_directory();
    let real_cargo_out_dir = fs::canonicalize(cargo_out_dir)?;
    let mut out_dir: &Path = &real_cargo_out_dir;

    // since target directory can be overridden in nightly builds, we need a heuristic to detect it
    // We check if the directory contains .rustc_info.json OR if the parent directory contains a Cargo.toml
    loop {
        let contains_rustc_info = directory_contains(out_dir, ".rustc_info.json")?;
        let parent = out_dir.parent();

        let parent_contains_cargo_toml = || {
            parent
                .is_some_and(|parent| directory_contains(parent, "Cargo.toml").is_ok_and(identity))
        };

        if contains_rustc_info || parent_contains_cargo_toml() {
            return Ok(out_dir.into());
        }

        match parent {
            Some(parent) => out_dir = parent,
            None => return Ok(real_cargo_out_dir),
        }
    }
}

fn directory_contains(directory: &Path, file_name: &str) -> io::Result<bool> {
    Ok(fs::read_dir(directory)?
        .find(|e| e.as_ref().is_ok_and(|e| e.file_name() == file_name))
        .is_some())
}

fn copy_file(file_name: &Path, from_dir: &Path, to_dir: &Path) -> io::Result<()> {
    let from_path = from_dir.join(file_name);
    let to_path = to_dir.join(file_name);
    if from_dir == to_dir {
        return Ok(());
    }

    fs::copy(&from_path, &to_path).map(|_| ()).map_err(|e| {
        eprintln!(
            "COPY OPERATION FAILED: from '{}' to '{}': {}",
            from_path.display(),
            to_path.display(),
            e
        );
        e
    })
}
