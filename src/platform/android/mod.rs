mod hooks;
pub mod storage;
use self::storage::{parse_storage_location, StorageLocation};

use super::errors::HookError;
use libc::c_void;
use plt_rs::{collect_modules, DynamicLibrary, DynamicSymbols};
use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::{fs, mem, ptr};
static EXT_JNI_PATH: OnceLock<String> = OnceLock::new();
pub unsafe fn get_current_username() -> Option<String> {
    let amt = match libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) {
        n if n < 0 => 512_usize,
        n => n as usize,
    };
    let mut buf = Vec::with_capacity(amt);
    let mut passwd: libc::passwd = mem::zeroed();
    let mut result = ptr::null_mut();
    match libc::getpwuid_r(
        libc::getuid(),
        &mut passwd,
        buf.as_mut_ptr(),
        buf.capacity(),
        &mut result,
    ) {
        0 if !result.is_null() => {
            let ptr = passwd.pw_name as *const _;
            let bytes = CStr::from_ptr(ptr).to_str().unwrap().to_owned();
            Some(bytes)
        }
        _ => None,
    }
}

#[no_mangle]
extern "C" fn Java_com_mojang_minecraftpe_MainActivity_give_storage_path_to_rust(
    mut env: jni::JNIEnv,
    thiz: jni::objects::JClass,
    string: jni::objects::JString,
) {
    let string = env.get_string(&string).expect("Cant get string from jni");
    let path = string.to_str().unwrap().to_owned();
    EXT_JNI_PATH.set(path).unwrap();
}

pub fn get_storage_location(options_path: &Path) -> Option<StorageLocation> {
    let int = match parse_storage_location(options_path) {
        Ok(location) => location,
        Err(e) => {
            log::info!("Cant parse storage: {e}");
            return None;
        }
    };
    StorageLocation::from_i8(int)
}
pub fn parse_current_aid(name: String) -> Option<i64> {
    name.strip_prefix('u')
        .and_then(|n| n.split_once('_').map(|(s, _)| s.parse::<i64>().unwrap()))
}
pub fn setup_logging() {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Trace),
    );
}
// Get the full path for a storage location
pub fn get_storage_path(location: StorageLocation) -> std::path::PathBuf {
    let pkgname = fs::read_to_string("/proc/self/cmdline").unwrap();
    log::info!("Package id: {pkgname}");
    //pkgnames are only ascii
    let username = unsafe { get_current_username().unwrap() };
    log::info!("Unix username: {username}");
    let userid = parse_current_aid(username).unwrap();
    log::info!("User id: {userid}");
    let pkgtrim = pkgname.trim_matches(char::from(0));
    let result = match location {
        StorageLocation::Internal => format!("/data/user/{userid}/") + pkgtrim,
        StorageLocation::External => {
            if let Some(path) = EXT_JNI_PATH.get() {
                log::info!("Jni path is availible, using it");
                path.to_owned()
            } else {
                format!("/storage/emulated/{}", userid) + "/Android/data/" + pkgtrim + "/files/"
            }
        }
    };
    result.into()
}

// Get app directory for the current platform
pub fn get_path() -> std::path::PathBuf {
    get_storage_path(StorageLocation::Internal)
}
// Setup asset hooks
pub fn setup_hooks() -> Result<(), HookError> {
    const LIBNAME: &str = "libminecraftpe";
    let lib_entry = match find_lib(LIBNAME) {
        Some(lib) => lib,
        None => return Err(HookError::MissingLib(LIBNAME.to_string())),
    };
    let dyn_lib = match DynamicLibrary::initialize(lib_entry) {
        Ok(lib) => lib,
        Err(e) => return Err(HookError::OsError(format!("{e}"))),
    };
    replace_plt_functions(
        &dyn_lib,
        &[
            ("AAssetManager_open", hooks::asset_open as *const _),
            ("AAsset_read", hooks::asset_read as *const _),
            ("AAsset_close", hooks::asset_close as *const _),
            ("AAsset_seek", hooks::asset_seek as *const _),
            ("AAsset_seek64", hooks::asset_seek64 as *const _),
            ("AAsset_getLength", hooks::asset_length as *const _),
            ("AAsset_getLength64", hooks::asset_length64 as *const _),
            (
                "AAsset_getRemainingLength",
                hooks::asset_remaining as *const _,
            ),
            (
                "AAsset_getRemainingLength64",
                hooks::asset_remaining64 as *const _,
            ),
            (
                "AAsset_openFileDescriptor",
                hooks::asset_fd_dummy as *const _,
            ),
            (
                "AAsset_openFileDescriptor64",
                hooks::asset_fd_dummy64 as *const _,
            ),
            ("AAsset_getBuffer", hooks::asset_get_buffer as *const _),
            ("AAsset_isAllocated", hooks::asset_is_alloc as *const _),
        ],
    )?;
    log::info!("Finished Hooking");
    Ok(())
}

fn find_lib<'a>(target_name: &str) -> Option<plt_rs::LoadedLibrary<'a>> {
    let loaded_modules = collect_modules();
    loaded_modules
        .into_iter()
        .find(|lib| lib.name().contains(target_name))
}
#[cfg(target_pointer_width = "32")]
fn try_find_function<'a>(
    dyn_lib: &'a DynamicLibrary,
    dyn_symbols: &'a DynamicSymbols,
    target_name: &str,
) -> Option<&'a plt_rs::elf32::DynRel> {
    let string_table = dyn_lib.string_table();
    if let Some(dyn_relas) = dyn_lib.relocs() {
        let dyn_relas = dyn_relas.entries().iter();
        if let Some(symbol) = dyn_relas
            .flat_map(|e| {
                dyn_symbols
                    .resolve_name(e.symbol_index() as usize, string_table)
                    .map(|s| (e, s))
            })
            .find(|(_, s)| s == target_name)
            .map(|(target_function, _)| target_function)
        {
            return Some(symbol);
        }
    }

    if let Some(dyn_relas) = dyn_lib.plt_rel() {
        let dyn_relas = dyn_relas.entries().iter();
        if let Some(symbol) = dyn_relas
            .flat_map(|e| {
                dyn_symbols
                    .resolve_name(e.symbol_index() as usize, string_table)
                    .map(|s| (e, s))
            })
            .find(|(_, s)| s == target_name)
            .map(|(target_function, _)| target_function)
        {
            return Some(symbol);
        }
    }
    None
}

/// Finding target function differs on 32 bit and 64 bit.
/// On 64 bit we want to check the addended relocations table only, opposed to the addendless relocations table.
/// Additionally, we will fall back to the plt given it is an addended relocation table.
#[cfg(target_pointer_width = "64")]
fn try_find_function<'a>(
    dyn_lib: &'a DynamicLibrary,
    dyn_symbols: &'a DynamicSymbols,
    target_name: &str,
) -> Option<&'a plt_rs::elf64::DynRela> {
    let string_table = dyn_lib.string_table();
    if let Some(dyn_relas) = dyn_lib.addend_relocs() {
        let dyn_relas = dyn_relas.entries().iter();
        if let Some(symbol) = dyn_relas
            .flat_map(|e| {
                dyn_symbols
                    .resolve_name(e.symbol_index() as usize, string_table)
                    .map(|s| (e, s))
            })
            .find(|(_, s)| s == target_name)
            .map(|(target_function, _)| target_function)
        {
            return Some(symbol);
        }
    }

    if let Some(dyn_relas) = dyn_lib.plt_rela() {
        let dyn_relas = dyn_relas.entries().iter();
        if let Some(symbol) = dyn_relas
            .flat_map(|e| {
                dyn_symbols
                    .resolve_name(e.symbol_index() as usize, string_table)
                    .map(|s| (e, s))
            })
            .find(|(_, s)| s == target_name)
            .map(|(target_function, _)| target_function)
        {
            return Some(symbol);
        }
    }
    None
}
fn replace_plt_functions(
    dyn_lib: &DynamicLibrary,
    functions: &[(&str, *const ())],
) -> Result<(), HookError> {
    let base_addr = dyn_lib.library().addr();
    for (fn_name, replacement) in functions {
        let Some(fn_plt) = try_find_function(dyn_lib, dyn_lib.symbols().unwrap(), fn_name) else {
            log::warn!("Missing symbol: {fn_name}");
            continue;
        };
        log::info!("Hooking {}...", fn_name);
        replace_plt_function(base_addr, fn_plt.r_offset as usize, *replacement)?;
    }
    log::info!("Hooked {} functions.", functions.len());
    Ok(())
}
fn replace_plt_function(
    base_addr: usize,
    offset: usize,
    replacement: *const (),
) -> Result<*const c_void, HookError> {
    let plt_fn_ptr = (base_addr + offset) as *mut *mut c_void;
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGE_SIZE) as usize };
    let plt_page = ((plt_fn_ptr as usize / page_size) * page_size) as *mut c_void;
    println!("page start for function is {plt_page:#X?}");
    unsafe {
        // Set the memory page to read, write
        let prot_res = libc::mprotect(plt_page, page_size, libc::PROT_WRITE | libc::PROT_READ);
        if prot_res != 0 {
            println!("Protection edit result: {prot_res}");
            return Err(HookError::OsError(
                "Mprotect error on setting rw".to_string(),
            ));
        }

        // Replace the function address
        let previous_address = std::ptr::replace(plt_fn_ptr, replacement as *mut _);

        // Set the memory page protection back to read only
        let prot_res = libc::mprotect(plt_page, page_size, libc::PROT_READ);
        if prot_res != 0 {
            return Err(HookError::OsError(
                "Mprotect error on setting read only".to_string(),
            ));
        }

        Ok(previous_address as *const c_void)
    }
}
