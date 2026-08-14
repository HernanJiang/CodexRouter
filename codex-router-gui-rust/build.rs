use std::{env, fs, path::PathBuf};

fn windows_manifest(assembly_version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity
      name="HernanJiang.CodexRouter"
      processorArchitecture="*"
      type="win32"
      version="{assembly_version}" />
  <description>Codex-Router</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2, PerMonitor</dpiAwareness>
    </windowsSettings>
  </application>
</assembly>
"#
    )
}

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let package_version = env::var("CARGO_PKG_VERSION").expect("Cargo package version is missing");
    let mut version_parts = package_version
        .split('-')
        .next()
        .unwrap_or(&package_version)
        .split('.')
        .map(|part| {
            part.parse::<u16>()
                .expect("invalid numeric version component")
        })
        .collect::<Vec<_>>();
    assert!(version_parts.len() <= 4, "version has too many components");
    version_parts.resize(4, 0);

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let icon_path = manifest_dir.join("assets/logo.ico");
    let output_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let manifest_path = output_dir.join("codex-router.manifest");
    let resource_path = output_dir.join("codex-router.rc");
    let icon_resource_path = icon_path.to_string_lossy().replace('\\', "/");
    let manifest_resource_path = manifest_path.to_string_lossy().replace('\\', "/");
    let version_commas = version_parts
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let assembly_version = version_parts
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(".");
    fs::write(&manifest_path, windows_manifest(&assembly_version))
        .expect("failed to write Windows application manifest");
    let resource = format!(
        r#"IDI_ICON1 ICON "{icon_resource_path}"

1 24 "{manifest_resource_path}"

1 VERSIONINFO
FILEVERSION {version_commas}
PRODUCTVERSION {version_commas}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904B0"
        BEGIN
            VALUE "LegalCopyright", "Copyright (c) 2026 Hernan_JIANG. All rights reserved.\0"
            VALUE "CompanyName", "Hernan_JIANG\0"
            VALUE "FileDescription", "Codex-Router\0"
            VALUE "FileVersion", "{package_version}\0"
            VALUE "InternalName", "Codex-Router\0"
            VALUE "OriginalFilename", "Codex-Router.exe\0"
            VALUE "ProductName", "Codex-Router\0"
            VALUE "ProductVersion", "{package_version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0409, 1200
    END
END
"#
    );
    fs::write(&resource_path, resource).expect("failed to write Windows resources");

    println!("cargo:rerun-if-changed=assets/logo.ico");
    println!("cargo:rerun-if-changed=build.rs");
    windres::Build::new().compile(resource_path).unwrap();
}
