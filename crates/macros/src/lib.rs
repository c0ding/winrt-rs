mod import;

use import::ImportMacro;
use proc_macro::TokenStream;
use quote::quote;
use syn::parse_macro_input;

/// A macro for generating WinRT modules into the current module.
///
/// This macro can be used to import WinRT APIs from OS dependencies as well
/// as NuGet packages. Use the `import` macro to directly include the generated code
/// into any module.
///
/// # Usage
/// To use, first specify which dependencies you are relying on. This can be both
/// `os` for depending on WinRT metadata shipped with Windows or `nuget: My.Package`
/// for NuGet packages.
///
/// ## NuGet
/// NuGet dependencies are expected in a well defined place. The `winmd` metadata files
/// should be in the cargo workspace's `target` directory in a subdirectory `nuget\My.Package`
/// where `My.Package` is the name of the NuGet package.
///
/// Any DLLs needed for the NuGet package to work should be next to work must be next to the final
/// executable.
///
/// Instead of handling this yourself, you can use the [`cargo winrt`](https://github.com/microsoft/winrt-rs/tree/master/crates/cargo-winrt)
/// helper subcommand.
///
/// ## Types
/// After specifying the dependencies, you must then specify which types you want to use. These
/// follow the same convention as Rust `use` paths. Types know which other types they depend on so
/// `import` will generate any other WinRT types needed for the specified type to work.
///
/// # Example
/// The following `import!` depends on both `os` metadata (i.e., metadata shipped on Windows 10), as well
/// as a 3rd-party NuGet dependency. It then generates all types inside of the `microsoft::ai::machine_learning`
/// namespace.
///
/// ```rust,ignore
/// import!(
///     dependencies
///         os
///         nuget: Microsoft.AI.MachineLearning
///     types
///         microsoft::ai::machine_learning::*
/// );
/// ```
#[proc_macro]
pub fn import(stream: TokenStream) -> TokenStream {
    let import = parse_macro_input!(stream as ImportMacro);
    import.to_tokens().into()
}

/// A macro for generating WinRT modules to a .rs file at build time.
///
/// This macro can be used to import WinRT APIs from OS dependencies as well
/// as NuGet packages. It is only intended for use from a crate's build.rs script.
///
/// The macro generates a single `build` function which can be used in build scripts
/// to generate the WinRT bindings. After using the `build` macro, call the
/// generated `build` function somewhere in the build.rs script's main function.
///
/// # Usage
/// To use, first specify which dependencies you are relying on. This can be both
/// `os` for depending on WinRT metadata shipped with Windows or `nuget: My.Package`
/// for NuGet packages.
///
/// ## NuGet
/// NuGet dependencies are expected in a well defined place. The `winmd` metadata files
/// should be in the cargo workspace's `target` directory in a subdirectory `nuget\My.Package`
/// where `My.Package` is the name of the NuGet package.
///
/// Any DLLs needed for the NuGet package to work should be next to work must be next to the final
/// executable.
///
/// Instead of handling this yourself, you can use the [`cargo winrt`](https://github.com/microsoft/winrt-rs/tree/master/crates/cargo-winrt)
/// helper subcommand.
///
/// ## Types
/// After specifying the dependencies, you must then specify which types you want to use. These
/// follow the same convention as Rust `use` paths. Types know which other types they depend on so
/// `import` will generate any other WinRT types needed for the specified type to work.
///
/// # Example
/// The following `build!` depends on both `os` metadata (i.e., metadata shipped on Windows 10), as well
/// as a 3rd-party NuGet dependency. It then generates all types inside of the `microsoft::ai::machine_learning`
/// namespace.
///
/// ```rust,ignore
/// build!(
///     dependencies
///         os
///         nuget: Microsoft.AI.MachineLearning
///     types
///         microsoft::ai::machine_learning::*
/// );
/// ```
#[proc_macro]
pub fn build(stream: TokenStream) -> TokenStream {
    let import = parse_macro_input!(stream as ImportMacro);
    let winmd_paths = import.winmd_paths().iter().map(|p| p.display().to_string());

    let change_if = quote! {
        #(println!("cargo:rerun-if-changed={}", #winmd_paths);)*
    };

    let tokens = match import.to_tokens_string() {
        Ok(t) => t,
        Err(t) => return t.into(),
    };

    let tokens = quote! {
        fn build() {
            use ::std::io::Write;
            let mut path = ::std::path::PathBuf::from(
                ::std::env::var("OUT_DIR").expect("No `OUT_DIR` env variable set"),
            );

            path.push("winrt.rs");
            let mut file = ::std::fs::File::create(&path).expect("Failed to create winrt.rs");

            let mut cmd = ::std::process::Command::new("rustfmt");
            cmd.arg("--emit").arg("stdout");
            cmd.stdin(::std::process::Stdio::piped());
            cmd.stdout(::std::process::Stdio::piped());
            {
                let child = cmd.spawn().unwrap();
                let mut stdin = child.stdin.unwrap();
                let stdout = child.stdout.unwrap();

                let t = ::std::thread::spawn(move || {
                    let mut s = stdout;
                    ::std::io::copy(&mut s, &mut file).unwrap();
                });

                #change_if

                writeln!(&mut stdin, "{}", #tokens).unwrap();
                // drop stdin to close that end of the pipe
                ::std::mem::drop(stdin);

                t.join().unwrap();
            }

            let status = cmd.status().unwrap();
            assert!(status.success(), "Could not successfully build");
        }
    };
    tokens.into()
}
