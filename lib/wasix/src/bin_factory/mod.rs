use std::{
    collections::HashMap,
    future::Future,
    ops::Deref,
    path::Path,
    pin::Pin,
    sync::{Arc, RwLock},
};

use anyhow::Context;
use exec::spawn_exec_wasm;
use virtual_fs::{AsyncReadExt, FileSystem};
use wasmer::FunctionEnvMut;
use wasmer_wasix_types::wasi::Errno;
use webc::Container;

mod binary_package;
mod exec;

pub use self::{
    binary_package::*,
    exec::{
        run_exec, spawn_exec, spawn_exec_module, spawn_load_module, spawn_load_wasm, spawn_union_fs,
    },
};
use crate::{
    os::{command::Commands, task::TaskJoinHandle},
    Runtime, SpawnError, WasiEnv,
};

#[derive(Debug, Clone)]
pub struct BinFactory {
    pub(crate) commands: Commands,
    runtime: Arc<dyn Runtime + Send + Sync + 'static>,
    pub(crate) local: Arc<RwLock<HashMap<String, Option<BinaryPackage>>>>,
}

impl BinFactory {
    pub fn new(runtime: Arc<dyn Runtime + Send + Sync + 'static>) -> BinFactory {
        BinFactory {
            commands: Commands::new_with_builtins(runtime.clone()),
            runtime,
            local: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn runtime(&self) -> &(dyn Runtime + Send + Sync) {
        self.runtime.deref()
    }

    pub fn set_binary(&self, name: &str, binary: BinaryPackage) {
        let mut cache = self.local.write().unwrap();
        cache.insert(name.to_string(), Some(binary));
    }

    #[allow(clippy::await_holding_lock)]
    pub async fn get_binary(
        &self,
        name: &str,
        fs: Option<&dyn FileSystem>,
    ) -> Option<BinaryPackage> {
        let name = name.to_string();

        // Fast path
        {
            let cache = self.local.read().unwrap();
            if let Some(data) = cache.get(&name) {
                return data.clone();
            }
        }

        // Slow path
        let mut cache = self.local.write().unwrap();

        // Check the cache
        if let Some(data) = cache.get(&name) {
            return data.clone();
        }

        // Check the filesystem for the file
        if name.starts_with('/') {
            if let Some(fs) = fs {
                match load_package_from_filesystem(fs, name.as_ref(), self.runtime()).await {
                    Ok(pkg) => {
                        cache.insert(name, Some(pkg.clone()));
                        return Some(pkg);
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = name,
                            error = &*e,
                            "Unable to load the package from disk"
                        );
                    }
                }
            }
        }

        // NAK
        cache.insert(name, None);
        None
    }

    pub fn spawn<'a>(
        &'a self,
        name: String,
        store: wasmer::Store,
        env: WasiEnv,
    ) -> Pin<Box<dyn Future<Output = Result<TaskJoinHandle, SpawnError>> + 'a>> {
        Box::pin(async move {
            // Find the binary (or die trying) and make the spawn type
            let res = self
                .get_binary(name.as_str(), Some(env.fs_root()))
                .await
                .ok_or_else(|| SpawnError::BinaryNotFound {
                    binary: name.clone(),
                });
            if res.is_err() {
                env.on_exit(Some(Errno::Noent.into())).await;
            }
            let binary = res?;

            // Execute
            spawn_exec(binary, name.as_str(), store, env, &self.runtime).await
        })
    }

    pub fn try_built_in(
        &self,
        name: String,
        parent_ctx: Option<&FunctionEnvMut<'_, WasiEnv>>,
        store: &mut Option<wasmer::Store>,
        builder: &mut Option<WasiEnv>,
    ) -> Result<TaskJoinHandle, SpawnError> {
        // We check for built in commands
        if let Some(parent_ctx) = parent_ctx {
            if self.commands.exists(name.as_str()) {
                return self
                    .commands
                    .exec(parent_ctx, name.as_str(), store, builder);
            }
        } else if self.commands.exists(name.as_str()) {
            tracing::warn!("builtin command without a parent ctx - {}", name);
        }
        Err(SpawnError::BinaryNotFound { binary: name })
    }

    pub fn spawn_executable<'a>(
        &'a self,
        name: String,
        store: wasmer::Store,
        env: WasiEnv,
    ) -> Pin<Box<dyn Future<Output = Result<TaskJoinHandle, SpawnError>> + 'a>> {
        Box::pin(async move {
            // Find the binary (or die trying) and make the spawn type
            let res = self
                .get_executable(name.as_str(), Some(env.fs_root()))
                .await
                .ok_or_else(|| SpawnError::BinaryNotFound {
                    binary: name.clone(),
                });
            if res.is_err() {
                env.on_exit(Some(Errno::Noent.into())).await;
            }
            let executable = res?;

            // Execute
            match executable {
                Executable::Wasm(bytes) => {
                    spawn_exec_wasm(&bytes, name.as_str(), env, &self.runtime).await
                }
                Executable::BinaryPackage(pkg) => {
                    spawn_exec(pkg, name.as_str(), store, env, &self.runtime).await
                }
            }
        })
    }

    // TODO: remove allow once BinFactory is refactored
    // currently fine because a BinFactory is only used by a single process tree
    #[allow(clippy::await_holding_lock)]
    pub async fn get_executable(
        &self,
        name: &str,
        fs: Option<&dyn FileSystem>,
    ) -> Option<Executable> {
        let name = name.to_string();

        // Fast path
        {
            let cache = self.local.read().unwrap();
            if let Some(data) = cache.get(&name) {
                data.clone().map(Executable::BinaryPackage);
            }
        }

        // Slow path
        let mut cache = self.local.write().unwrap();

        // Check the cache
        if let Some(data) = cache.get(&name) {
            return data.clone().map(Executable::BinaryPackage);
        }

        // Check the filesystem for the file
        if name.starts_with('/') {
            if let Some(fs) = fs {
                match load_executable_from_filesystem(fs, name.as_ref(), self.runtime()).await {
                    Ok(executable) => {
                        if let Executable::BinaryPackage(pkg) = &executable {
                            cache.insert(name, Some(pkg.clone()));
                        }

                        return Some(executable);
                    }
                    Err(e) => {
                        tracing::warn!(
                            path = name,
                            error = &*e,
                            "Unable to load the package from disk"
                        );
                    }
                }
            }
        }

        // NAK
        cache.insert(name, None);
        None
    }
}

async fn load_package_from_filesystem(
    fs: &dyn FileSystem,
    path: &Path,
    rt: &(dyn Runtime + Send + Sync),
) -> Result<BinaryPackage, anyhow::Error> {
    let mut f = fs
        .new_open_options()
        .read(true)
        .open(path)
        .context("Unable to open the file")?;

    let mut data = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut data).await.context("Read failed")?;

    let container = Container::from_bytes(data).context("Unable to parse the WEBC file")?;
    let pkg = BinaryPackage::from_webc(&container, rt)
        .await
        .context("Unable to load the package")?;

    Ok(pkg)
}

pub enum Executable {
    Wasm(bytes::Bytes),
    BinaryPackage(BinaryPackage),
}

async fn load_executable_from_filesystem(
    fs: &dyn FileSystem,
    path: &Path,
    rt: &(dyn Runtime + Send + Sync),
) -> Result<Executable, anyhow::Error> {
    let mut f = fs
        .new_open_options()
        .read(true)
        .open(path)
        .context("Unable to open the file")?;

    let mut data = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut data).await.context("Read failed")?;

    let bytes: bytes::Bytes = data.into();

    if let Ok(container) = Container::from_bytes(bytes.clone()) {
        let pkg = BinaryPackage::from_webc(&container, rt)
            .await
            .context("Unable to load the package")?;

        Ok(Executable::BinaryPackage(pkg))
    } else {
        Ok(Executable::Wasm(bytes))
    }
}
