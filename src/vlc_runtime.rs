/*
* Copyright (c) 2025 xiSage
*
* This library is free software; you can redistribute it and/or
* modify it under the terms of the GNU Lesser General Public
* License as published by the Free Software Foundation; either
* version 2.1 of the License, or (at your option) any later version.
*
* This library is distributed in the hope that it will be useful,
* but WITHOUT ANY WARRANTY; without even the implied warranty of
* MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
* Lesser General Public License for more details.
*
* You should have received a copy of the GNU Lesser General Public
* License along with this library; if not, write to the Free Software
* Foundation, Inc., 51 Franklin Street, Fifth Floor, Boston, MA  02110-1301
* USA
*/

//! Makes the bundled LibVLC find its own plugin directory.
//!
//! LibVLC derives that directory from the location of the module that contains
//! `config_GetLibDir`:
//!
//! * Linux parses `/proc/self/maps`, finds the mapping holding that symbol and
//!   keeps its **directory**, then appends `vlc/plugins`;
//! * Windows does the equivalent with `VirtualQuery` + `GetModuleFileNameW` and
//!   appends `plugins`.
//!
//! The file name is never consulted, only its directory. That has two
//! consequences worth stating plainly:
//!
//! * relocating the runtime does not make the derivation fail, it makes it
//!   silently produce a directory that may not exist. LibVLC then loads zero
//!   modules and reports nothing; the failure only surfaces much later as
//!   "Codec not supported";
//! * if `/proc/self/maps` cannot be read, LibVLC falls back to a path compiled
//!   in at build time, so the answer depends on the deployment environment.
//!
//! Three overrides are needed, not one. LibVLC derives the plugin directory, the
//! libexec directory (the out-of-process preparser and the plugin cache
//! generator) and the data directory (the Lua scripts) the same way, and each has
//! its own variable:
//!
//! | variable            | value                  |
//! |---------------------|------------------------|
//! | `VLC_LIB_PATH`      | `<addon bin dir>/vlc`  |
//! | `VLC_LIBEXEC_PATH`  | `<addon bin dir>/libexec/vlc` |
//! | `VLC_DATA_PATH`     | `<addon bin dir>/share/vlc`   |
//!
//! Missing the last two is not obvious: every plugin still loads, and the
//! failure only appears when something tries to use them ("Fail to create Process
//! in process_pool", then "cannot start any interface"). They exist in LibVLC 4
//! only, which is the version this extension targets.
//!
//! Windows is deliberately left alone: the addon already ships
//! `libvlccore.dll` next to a `plugins` directory, which is exactly what the
//! Windows derivation produces, and it is known to work.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use godot::prelude::*;

/// Environment variable LibVLC 4 reads to override its library directory.
pub const VLC_LIB_PATH: &str = "VLC_LIB_PATH";

/// Environment variable LibVLC 4 reads to override its libexec directory.
pub const VLC_LIBEXEC_PATH: &str = "VLC_LIBEXEC_PATH";

/// Environment variable LibVLC 4 reads to override its data directory.
pub const VLC_DATA_PATH: &str = "VLC_DATA_PATH";

/// Each override, and the directory it points at relative to the addon's binary
/// directory. The two directories beyond `lib` sit beside it in VLC's install
/// layout, which is why they are not inside `vlc/`.
// Read by `configure_vlc_paths`, which only does anything on Linux.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const PATH_OVERRIDES: [(&str, &str); 3] = [
    (VLC_LIB_PATH, "vlc"),
    (VLC_LIBEXEC_PATH, "libexec/vlc"),
    (VLC_DATA_PATH, "share/vlc"),
];

/// Decides whether, and to what, one override should be set.
///
/// Returns `None` when nothing should be done: either the variable is already
/// set to something (packagers and users may have pointed LibVLC somewhere on
/// purpose, and that choice is respected), or the directory holding this
/// library could not be determined.
// `configure_vlc_paths` is Linux-only, so on other platforms this is reachable
// only from the tests.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn resolve_path_override(
    module_dir: Option<&Path>,
    current: Option<&OsStr>,
    subdir: &str,
) -> Option<PathBuf> {
    if current.is_some_and(|value| !value.is_empty()) {
        return None;
    }

    let module_dir = module_dir?;
    if module_dir.as_os_str().is_empty() {
        return None;
    }

    Some(module_dir.join(subdir))
}

/// Finds the directory of the mapping that contains `own_address`.
///
/// This mirrors what LibVLC itself does, so the path we pass explicitly agrees
/// with the path LibVLC would otherwise have derived.
///
/// The parser is kept free of any system calls so the format handling can be
/// tested without a Linux host.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn own_module_dir_from_maps(maps: &str, own_address: usize) -> Option<PathBuf> {
    for line in maps.lines() {
        let mut fields = line.split_whitespace();

        let Some(range) = fields.next() else { continue };
        let Some((start, end)) = range.split_once('-') else {
            continue;
        };
        let (Ok(start), Ok(end)) = (
            usize::from_str_radix(start, 16),
            usize::from_str_radix(end, 16),
        ) else {
            continue;
        };
        if own_address < start || own_address >= end {
            continue;
        }

        // Layout: range perms offset dev inode pathname
        let Some(pathname) = fields.nth(4) else {
            continue; // anonymous mapping, no pathname at all
        };
        if !pathname.starts_with('/') {
            continue; // [heap], [stack], [vdso], ...
        }

        return Path::new(pathname).parent().map(Path::to_path_buf);
    }

    None
}

/// Points the LibVLC path overrides at the directories shipped next to this
/// library. Variables that are already set are left alone.
#[cfg(target_os = "linux")]
pub fn configure_vlc_paths() {
    use std::ffi::CString;

    let module_dir = own_module_dir();

    for (variable, subdir) in PATH_OVERRIDES {
        let current = std::env::var_os(variable);
        let Some(path) = resolve_path_override(module_dir.as_deref(), current.as_deref(), subdir)
        else {
            continue;
        };

        let (Ok(name), Ok(value)) = (
            CString::new(variable),
            CString::new(path.as_os_str().as_encoded_bytes()),
        ) else {
            godot_error!("godot-vlc: {variable} could not be set to {path:?}");
            continue;
        };

        // libc::setenv rather than std::env::set_var: mutating the environment is
        // unsafe in Rust's model because it races with other threads, and LibVLC
        // reads the environment through the C library regardless.
        let result = unsafe { libc::setenv(name.as_ptr(), value.as_ptr(), 1) };
        if result != 0 {
            godot_error!("godot-vlc: failed to set {variable} to {path:?}");
        } else {
            godot_print!("godot-vlc: {variable}={}", path.display());
        }
    }
}

/// Directory of the module that holds the address used for identification.
#[cfg(target_os = "linux")]
fn own_module_dir() -> Option<PathBuf> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    // A function defined in this library identifies this library's mapping.
    let own_address = own_module_dir_from_maps as usize;
    own_module_dir_from_maps(&maps, own_address)
}

/// No-op: see the module documentation for why Windows needs no override.
#[cfg(not(target_os = "linux"))]
pub fn configure_vlc_paths() {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// A `/proc/self/maps` excerpt with a fixed layout, alongside the shapes the
    /// parser has to skip: an anonymous mapping, a bracketed pseudo-mapping and
    /// the stack.
    ///
    /// The layout is fixed rather than built around the address under test: a
    /// fixture that brackets whatever it is asked about would make every lookup
    /// succeed and the test would prove nothing.
    fn maps_fixture() -> String {
        concat!(
            "7f2a00000000-7f2a00021000 r--p 00000000 08:01 1000 /usr/lib/libc.so.6\n",
            "7f2a10000000-7f2a10002000 r--p 00000000 00:00 0 \n",
            "7f2a20000000-7f2a20001000 r-xp 00000000 00:00 0 [vdso]\n",
            "7f2a30000000-7f2a30040000 r-xp 00000000 08:01 2000 /home/dev/addon/bin/linux-x64/libgodot_vlc.so\n",
            "7f2a40000000-7f2a40001000 r-xp 00000000 00:00 0 [heap]\n",
            "7ffd00000000-7ffd00021000 rw-p 00000000 00:00 0 [stack]\n",
        )
        .to_string()
    }

    #[test]
    fn override_points_at_the_directory_next_to_the_module() {
        let module_dir = Path::new("/addon/bin/linux-x64");
        assert_eq!(
            resolve_path_override(Some(module_dir), None, "vlc"),
            Some(PathBuf::from("/addon/bin/linux-x64/vlc"))
        );
    }

    #[test]
    fn every_override_has_a_distinct_subdirectory() {
        // Compared as relative paths, because Path::join uses the host separator
        // and an assertion on the rendered string would only hold on Unix.
        let module_dir = Path::new("/addon/bin/linux-x64");
        let resolved: Vec<PathBuf> = PATH_OVERRIDES
            .iter()
            .map(|(_, subdir)| {
                let path = resolve_path_override(Some(module_dir), None, subdir).unwrap();
                path.strip_prefix(module_dir).unwrap().to_path_buf()
            })
            .collect();
        assert_eq!(
            resolved,
            vec![
                PathBuf::from("vlc"),
                PathBuf::from("libexec").join("vlc"),
                PathBuf::from("share").join("vlc"),
            ],
            "the libexec and data overrides are easy to forget and their absence is silent"
        );
    }

    #[test]
    fn override_is_a_pure_join_and_keeps_the_parent() {
        let module_dir = Path::new("/deep/nested/place");
        let resolved = resolve_path_override(Some(module_dir), None, "vlc").unwrap();
        assert!(resolved.starts_with(module_dir));
        assert_eq!(resolved.file_name().unwrap(), "vlc");
        assert_eq!(resolved.parent().unwrap(), module_dir);
    }

    #[test]
    fn an_existing_value_is_never_overwritten() {
        let module_dir = Path::new("/addon/bin/linux-x64");
        let existing = OsString::from("/custom/lib/vlc");
        assert_eq!(
            resolve_path_override(Some(module_dir), Some(existing.as_os_str()), "vlc"),
            None
        );
    }

    #[test]
    fn an_empty_existing_value_counts_as_unset() {
        let module_dir = Path::new("/addon/bin/linux-x64");
        let empty = OsString::new();
        assert_eq!(
            resolve_path_override(Some(module_dir), Some(empty.as_os_str()), "vlc"),
            Some(PathBuf::from("/addon/bin/linux-x64/vlc"))
        );
    }

    #[test]
    fn no_module_directory_means_no_override() {
        assert_eq!(resolve_path_override(None, None, "vlc"), None);
        assert_eq!(
            resolve_path_override(Some(Path::new("")), None, "vlc"),
            None,
            "an empty module directory must not resolve to a bare relative 'vlc'"
        );
    }

    #[test]
    fn maps_parsing_finds_the_directory_of_our_own_mapping() {
        // An address inside the libgodot_vlc.so mapping of the fixture.
        let own_address = 0x7f2a_3001_0000_usize;
        assert_eq!(
            own_module_dir_from_maps(&maps_fixture(), own_address),
            Some(PathBuf::from("/home/dev/addon/bin/linux-x64"))
        );
    }

    #[test]
    fn maps_parsing_skips_anonymous_and_bracketed_mappings() {
        // anonymous, [vdso] and [heap] respectively
        for own_address in [0x7f2a_1000_0000_usize, 0x7f2a_2000_0000, 0x7f2a_4000_0000] {
            assert_eq!(
                own_module_dir_from_maps(&maps_fixture(), own_address),
                None,
                "address {own_address:#x} should not yield a directory"
            );
        }
    }

    #[test]
    fn maps_parsing_returns_none_for_an_unmapped_address() {
        assert_eq!(own_module_dir_from_maps(&maps_fixture(), 0x1000), None);
        assert_eq!(own_module_dir_from_maps("", 0x1000), None);
    }

    #[test]
    fn maps_parsing_ignores_malformed_lines() {
        let maps = "garbage\n7f2a-zz r-xp 0 0 0 /lib/x.so\n0-1 r-xp 0 0 0 \n";
        assert_eq!(own_module_dir_from_maps(maps, 0x7f2a), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn own_module_dir_resolves_on_a_real_linux_host() {
        let dir = own_module_dir().expect("/proc/self/maps should reveal our own module directory");
        assert!(dir.is_absolute(), "expected an absolute path, got {dir:?}");
    }
}
