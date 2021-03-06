use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use rayon::iter::ParallelIterator;
use syn::parse::{self, Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{parse_macro_input, Error, UseTree};

use winrt_deps::cargo;
use winrt_gen::{
    dependencies, NamespaceTypes, TypeLimit, TypeLimits, TypeReader, TypeStage, WinmdFile,
};

use std::convert::{TryFrom, TryInto};
use std::{collections::BTreeSet, path::PathBuf};

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
        #change_if
        use ::std::io::Write;
        let mut path = ::std::path::PathBuf::from(
            ::std::env::var("OUT_DIR").expect("No `OUT_DIR` env variable set"),
        );

        path.push("winrt.rs");
        let mut file = ::std::fs::File::create(&path).expect("Failed to create winrt.rs");
        file.write_all(#tokens.as_bytes()).expect("Could not write generated code to output file");

        let mut cmd = ::std::process::Command::new("rustfmt");
        cmd.arg(&path);
        let output = cmd.output();
        match output {
            Err(_) => eprintln!("Could not execute rustfmt"),
            Ok(o) if !o.status.success() => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                eprintln!("rustfmt did not exit properly: {:?}\n{}", o.status.code(), stderr);
            }
            _ => {}
        };
    };
    tokens.into()
}

/// A parsed `import!` macro
struct ImportMacro {
    foundation: bool,
    dependencies: Dependencies,
    types: TypesDeclarations,
}

impl ImportMacro {
    fn winmd_paths(&self) -> &BTreeSet<PathBuf> {
        &self.dependencies.0
    }

    fn to_tokens_string(self) -> Result<String, proc_macro2::TokenStream> {
        let dependencies = self.dependencies.0.iter().map(WinmdFile::new).collect();

        let reader = &TypeReader::new(dependencies);
        let mut limits = TypeLimits::new(reader);

        let foundation_namespaces = &[
            "Windows.Foundation",
            "Windows.Foundation.Collections",
            "Windows.Foundation.Diagnostics",
            "Windows.Foundation.Numerics",
        ];

        if self.foundation {
            for namespace in foundation_namespaces {
                limits
                    .insert(NamespaceTypes {
                        namespace: namespace.to_string(),
                        limit: TypeLimit::All,
                    })
                    .unwrap();
            }
        }

        for limit in self.types.0 {
            let types = limit.types;
            let syntax = limit.syntax;
            if let Err(e) = limits.insert(types).map_err(|ns| {
                syn::Error::new_spanned(syntax, format!("'{}' is not a known namespace", ns))
            }) {
                return Err(e.to_compile_error());
            };
        }

        let stage = TypeStage::from_limits(reader, &limits);
        let mut tree = stage.into_tree();

        if !self.foundation {
            for namespace in foundation_namespaces {
                tree.remove(namespace);
            }

            tree.reexport();
        }

        let ts = tree
            .to_tokens()
            .reduce(squote::TokenStream::new, |mut accum, n| {
                accum.combine(&n);
                accum
            });

        Ok(ts.into_string())
    }
}

impl Parse for ImportMacro {
    fn parse(input: ParseStream) -> parse::Result<Self> {
        let dependencies = Dependencies::parse()
            .map_err(|e| syn::Error::new(proc_macro2::Span::call_site(), format!("{}", e)))?;
        let foundation = input.parse::<keywords::foundation>().is_ok();
        let _ = input.parse::<keywords::types>()?;
        let types: TypesDeclarations = input.parse()?;

        Ok(ImportMacro {
            foundation,
            dependencies,
            types,
        })
    }
}

/// keywords used in the `build!` macro
mod keywords {
    syn::custom_keyword!(nuget);
    syn::custom_keyword!(dependencies);
    syn::custom_keyword!(types);
    syn::custom_keyword!(foundation);
}

/// A parsed `dependencies` section of the `import!` macro
#[derive(Debug)]
struct Dependencies(BTreeSet<PathBuf>);

impl Dependencies {
    fn parse() -> Result<Self, Box<dyn std::error::Error + 'static>> {
        let mut dependencies = BTreeSet::new();
        let deps = cargo::package_manifest()?.get_dependencies()?;
        for dep in deps {
            let nuget_path = std::fs::read_dir(dependencies::nuget_root())?;
            let name = dep.name();
            let mut dependency_path_iter = nuget_path
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.path().is_dir())
                .filter(|entry| {
                    if let Some(entry) = entry.file_name().to_str().map(str::to_owned) {
                        entry.starts_with(&name)
                    } else {
                        false
                    }
                })
                .map(|entry| entry.path());

            let path = dependency_path_iter
                .next()
                .ok_or_else(|| format!("No directory for dependency '{}'", dep.name()))?;
            // Check for multiple versions
            if dependency_path_iter.next().is_some() {
                return Err(format!("multiple nuget package versions found for '{}'", name).into());
            }

            dependencies::expand_paths(path, &mut dependencies, true)
                .map_err(|e| format!("could not read dependency: {}", e))?;
        }
        Ok(Dependencies(dependencies))
    }
}

/// A parsed `types` section of the `import!` macro
struct TypesDeclarations(BTreeSet<TypesDeclaration>);

struct TypesDeclaration {
    types: NamespaceTypes,
    syntax: syn::UseTree,
}
impl std::cmp::PartialOrd for TypesDeclaration {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl std::cmp::Ord for TypesDeclaration {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.types.cmp(&other.types)
    }
}
impl PartialEq for TypesDeclaration {
    fn eq(&self, other: &Self) -> bool {
        self.types == other.types
    }
}
impl Eq for TypesDeclaration {}

impl TryFrom<syn::UseTree> for TypesDeclaration {
    type Error = syn::Error;
    fn try_from(tree: UseTree) -> Result<Self, Self::Error> {
        Ok(Self {
            types: use_tree_to_namespace_types(&tree)?,
            syntax: tree,
        })
    }
}

impl Parse for TypesDeclarations {
    fn parse(input: ParseStream) -> parse::Result<Self> {
        let mut limits = BTreeSet::new();
        loop {
            if input.is_empty() {
                break;
            }

            let use_tree: syn::UseTree = input.parse()?;
            let limit: TypesDeclaration = use_tree.try_into()?;

            limits.insert(limit);
        }
        Ok(Self(limits))
    }
}

// Snake <-> camel casing is lossy so we go for character but not case conversion
// and deal with casing once we have an index of namespaces to compare against.
fn namespace_literal_to_rough_namespace(namespace: &str) -> String {
    let mut result = String::with_capacity(namespace.len());
    for c in namespace.chars() {
        if c != '"' && c != '_' {
            result.extend(c.to_lowercase());
        }
    }
    result
}

fn use_tree_to_namespace_types(use_tree: &syn::UseTree) -> parse::Result<NamespaceTypes> {
    fn recurse(tree: &UseTree, current: &mut String) -> parse::Result<NamespaceTypes> {
        fn check_for_module_instead_of_type(name: &str, span: Span) -> parse::Result<()> {
            let error = Err(Error::new(
                span,
                "Expected `*` or type name, but found what appears to be a module",
            ));
            if name.to_lowercase() == name {
                return error;
            }
            Ok(())
        }

        match tree {
            UseTree::Path(p) => {
                if !current.is_empty() {
                    current.push('.');
                }

                current.push_str(&p.ident.to_string());

                recurse(&*p.tree, current)
            }
            UseTree::Glob(_) => {
                let namespace = namespace_literal_to_rough_namespace(&current.clone());
                Ok(NamespaceTypes {
                    namespace,
                    limit: TypeLimit::All,
                })
            }
            UseTree::Group(g) => {
                let namespace = namespace_literal_to_rough_namespace(&current.clone());

                let mut types = Vec::with_capacity(g.items.len());
                for tree in &g.items {
                    match tree {
                        UseTree::Name(n) => {
                            let name = n.ident.to_string();
                            check_for_module_instead_of_type(&name, n.span())?;
                            types.push(name);
                        }
                        UseTree::Rename(_) => {
                            return Err(Error::new(tree.span(), "Renaming syntax is not supported"))
                        }
                        _ => return Err(Error::new(tree.span(), "Nested paths not allowed")),
                    }
                }
                Ok(NamespaceTypes {
                    namespace,
                    limit: TypeLimit::Some(types),
                })
            }
            UseTree::Name(n) => {
                let namespace = namespace_literal_to_rough_namespace(&current.clone());
                let name = n.ident.to_string();
                check_for_module_instead_of_type(&name, n.span())?;
                Ok(NamespaceTypes {
                    namespace,
                    limit: TypeLimit::Some(vec![name]),
                })
            }
            UseTree::Rename(r) => Err(Error::new(r.span(), "Renaming syntax is not supported")),
        }
    }

    recurse(use_tree, &mut String::new())
}
