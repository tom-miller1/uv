use anyhow::{Context, Result};
use owo_colors::OwoColorize;
use std::path::PathBuf;

use uv_cache::Cache;
use uv_client::Connectivity;
use uv_configuration::{Concurrency, ExportFormat, ExtrasSpecification, InstallOptions};
use uv_fs::CWD;
use uv_normalize::{PackageName, DEV_DEPENDENCIES};
use uv_python::{PythonDownloads, PythonPreference, PythonRequest};
use uv_resolver::RequirementsTxtExport;
use uv_workspace::{DiscoveryOptions, MemberDiscovery, VirtualProject, Workspace};

use crate::commands::pip::loggers::DefaultResolveLogger;
use crate::commands::project::lock::do_safe_lock;
use crate::commands::project::{FoundInterpreter, ProjectError};
use crate::commands::{pip, ExitStatus, OutputWriter};
use crate::printer::Printer;
use crate::settings::ResolverSettings;

/// Export the project's `uv.lock` in an alternate format.
#[allow(clippy::fn_params_excessive_bools)]
pub(crate) async fn export(
    format: ExportFormat,
    package: Option<PackageName>,
    hashes: bool,
    install_options: InstallOptions,
    output_file: Option<PathBuf>,
    extras: ExtrasSpecification,
    dev: bool,
    locked: bool,
    frozen: bool,
    python: Option<String>,
    settings: ResolverSettings,
    python_preference: PythonPreference,
    python_downloads: PythonDownloads,
    connectivity: Connectivity,
    concurrency: Concurrency,
    native_tls: bool,
    quiet: bool,
    cache: &Cache,
    printer: Printer,
) -> Result<ExitStatus> {
    // Identify the project.
    let project = if let Some(package) = package {
        VirtualProject::Project(
            Workspace::discover(&CWD, &DiscoveryOptions::default())
                .await?
                .with_current_project(package.clone())
                .with_context(|| format!("Package `{package}` not found in workspace"))?,
        )
    } else if frozen {
        VirtualProject::discover(
            &CWD,
            &DiscoveryOptions {
                members: MemberDiscovery::None,
                ..DiscoveryOptions::default()
            },
        )
        .await?
    } else {
        VirtualProject::discover(&CWD, &DiscoveryOptions::default()).await?
    };

    let VirtualProject::Project(project) = project else {
        return Err(anyhow::anyhow!("Legacy non-project roots are not supported in `uv export`; add a `[project]` table to your `pyproject.toml` to enable exports"));
    };

    // Find an interpreter for the project
    let interpreter = FoundInterpreter::discover(
        project.workspace(),
        python.as_deref().map(PythonRequest::parse),
        python_preference,
        python_downloads,
        connectivity,
        native_tls,
        cache,
        printer,
    )
    .await?
    .into_interpreter();

    // Lock the project.
    let lock = match do_safe_lock(
        locked,
        frozen,
        project.workspace(),
        &interpreter,
        settings.as_ref(),
        Box::new(DefaultResolveLogger),
        connectivity,
        concurrency,
        native_tls,
        cache,
        printer,
    )
    .await
    {
        Ok(result) => result.into_lock(),
        Err(ProjectError::Operation(pip::operations::Error::Resolve(
            uv_resolver::ResolveError::NoSolution(err),
        ))) => {
            let report = miette::Report::msg(format!("{err}")).context(err.header());
            anstream::eprint!("{report:?}");
            return Ok(ExitStatus::Failure);
        }
        Err(err) => return Err(err.into()),
    };

    // Include development dependencies, if requested.
    let dev = if dev {
        vec![DEV_DEPENDENCIES.clone()]
    } else {
        vec![]
    };

    // Write the resolved dependencies to the output channel.
    let mut writer = OutputWriter::new(!quiet || output_file.is_none(), output_file.as_deref());

    // Generate the export.
    match format {
        ExportFormat::RequirementsTxt => {
            let export = RequirementsTxtExport::from_lock(
                &lock,
                project.project_name(),
                &extras,
                &dev,
                hashes,
                &install_options,
            )?;
            writeln!(
                writer,
                "{}",
                "# This file was autogenerated via `uv export`.".green()
            )?;
            write!(writer, "{export}")?;
        }
    }

    writer.commit().await?;

    Ok(ExitStatus::Success)
}
