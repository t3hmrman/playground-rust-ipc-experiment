just := env_var_or_default("JUST", just_executable())

cargo := env_var_or_default("CARGO", "cargo")
cargo_args := env_var_or_default("CARGO_ARGS", "")
cargo_run_args := env_var_or_default("CARGO_RUN_ARGS", "")

build_mode := env_var_or_default("BUILD_MODE", "debug")
build_mode_cargo_args := if build_mode == "release" { "--release" } else { "" }

@default:
    {{just}} --list

# Format project code
@fmt:
    {{cargo}} fmt

# Lint the project
@lint:
    {{cargo}} clippy --all-targets --all-features

# Lint and fix problems
@lint-fix:
    {{cargo}} clippy --all-targets --all-features --fix --allow-dirty --allow-staged

# Build the project
@build:
    {{cargo}} {{cargo_args}} build {{build_mode_cargo_args}}

# Run all the experiments one after another
@run-all:
    {{just}} ipc-ipcc
    {{just}} ipc-shmem-shared-mem-queue
    {{just}} ipc-shmem-raw-sync

# Run the experimental IPC testing code (ipc-channel)
@ipc-ipcc: build
    {{just}} --justfile crates/ipc-benchmark/Justfile ipc-ipcc

# Run the experimental IPC testing code (shared-mem - shared-mem-queue)
@ipc-shmem-shared-mem-queue: build
    {{just}} --justfile crates/ipc-benchmark/Justfile ipc-shmem-shared-mem-queue

# Run the experimental IPC testing code (shared-mem - raw_sync)
@ipc-shmem-raw-sync: build
    {{just}} --justfile crates/ipc-benchmark/Justfile ipc-shmem-raw-sync
