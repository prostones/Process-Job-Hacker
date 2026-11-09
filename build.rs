fn main() {
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rerun-if-changed=app.admin.manifest");

    let profile = std::env::var("PROFILE").unwrap_or_default();
    let manifest = if profile == "release" {
        "app.admin.manifest"
    } else {
        "app.manifest"
    };

    let mut resource = winres::WindowsResource::new();
    resource.set_icon("assets/app.ico");
    resource.set_manifest_file(manifest);
    resource.set("FileDescription", "Process Job Hacker");
    resource.set("ProductName", "Process Job Hacker");
    resource.set("CompanyName", "prostones");
    resource.set("OriginalFilename", "process-job-hacker.exe");
    resource.set("InternalName", "process-job-hacker");
    resource.set(
        "LegalCopyright",
        "Copyright (c) 2026 prostones. MIT License.",
    );
    if let Err(error) = resource.compile() {
        println!("cargo:warning=winres failed: {error}");
    }
}
