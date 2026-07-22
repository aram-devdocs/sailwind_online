# Sailwind Online task runner.
#
# Scripts have a .ps1 (Windows) and a .sh (POSIX) mirror. This Makefile picks
# the right one per platform. Plain dotnet and cargo commands run the same on
# both. Default target is `help`.

.DEFAULT_GOAL := help

ifeq ($(OS),Windows_NT)
  runscript = powershell -NoProfile -ExecutionPolicy Bypass -File scripts/$(1).ps1
else
  runscript = bash scripts/$(1).sh
endif

DOTNET := dotnet
CARGO  := cargo
SLN    := SailwindOnline.sln
SLNF   := SailwindOnline.CI.slnf
SERVER_MANIFEST := server/Cargo.toml

.PHONY: help setup codegen contracts validate check audit build build-ci test \
        test-hooks template server-build server-run smoke test-profile deploy run-modded clean

help:  ## List available targets
	@echo Sailwind Online - make targets
	@echo.
	@echo   setup         Provision lib/ and the sandbox from a local Steam install
	@echo   codegen       Regenerate the Sailwind.API surface from the game assembly
	@echo   contracts     Regenerate FlatBuffers C# and Rust from contracts/fbs
	@echo   validate      Canonical gate mirroring CI: conventions, leak scan, check
	@echo   check         Fast gate: game-free build plus tests plus Rust checks
	@echo   audit         Full gate: check plus the game-coupled tier when lib/ is present
	@echo   build         Build the full solution in Release (needs lib/)
	@echo   build-ci      Build the game-free solution filter in Release
	@echo   test          Game-free dotnet tests plus cargo test
	@echo   test-hooks    Run the governance hook golden-fixture suite
	@echo   template      Install the dotnet new mod template and run its game-free test
	@echo   server-build  Build the Rust server workspace
	@echo   server-run    Run the Rust server (sailwind-online-server)
	@echo   smoke         Build the server in Release and run the protocol-smoke harness
	@echo   test-profile  Create the dedicated SailwindOnline Thunderstore test profile
	@echo   deploy        Build Release and deploy plugins to the test profile (opt-in)
	@echo   run-modded    Start the server and launch Sailwind modded from the test profile
	@echo   clean         Remove bin, obj, target, and artifacts

setup:  ## Provision lib/ and the sandbox from a local Steam install
	$(call runscript,setup-game)

codegen:  ## Regenerate the Sailwind.API surface from the game assembly
	$(call runscript,generate-api)

contracts:  ## Regenerate FlatBuffers C# and Rust from contracts/fbs
	$(call runscript,gen-contracts)

validate:  ## Canonical gate (mirrors CI): conventions, leak scan, game-free build, tests, Rust checks
	$(call runscript,check)

check:  ## Fast gate: game-free build plus tests plus Rust checks
	$(call runscript,check)

audit:  ## Full gate: check plus the game-coupled tier when lib/ is present
	$(call runscript,check)

build:  ## Build the full solution in Release (needs lib/)
	$(DOTNET) build $(SLN) -c Release

build-ci:  ## Build the game-free solution filter in Release
	$(DOTNET) build $(SLNF) -c Release

test:  ## Game-free dotnet tests plus cargo test
	$(DOTNET) test $(SLNF) -c Release
	$(CARGO) test --manifest-path $(SERVER_MANIFEST)

test-hooks:  ## Run the governance hook golden-fixture suite
	bash scripts/test-hooks.sh

template:  ## Install the dotnet new mod template locally and run its game-free verification test
	$(DOTNET) new install ./templates/sailwind-mod --force
	$(DOTNET) test tests/Sailwind.Templates.Tests -c Release

server-build:  ## Build the Rust server workspace
	$(CARGO) build --manifest-path $(SERVER_MANIFEST)

server-run:  ## Run the Rust server
	$(CARGO) run --manifest-path $(SERVER_MANIFEST) -p sailwind-online-server

smoke:  ## Build the server in Release and run the protocol-smoke harness
	$(CARGO) build --release --manifest-path $(SERVER_MANIFEST)
	$(DOTNET) run --project tools/protocol-smoke

test-profile:  ## Create the dedicated SailwindOnline Thunderstore test profile
	$(call runscript,setup-test-profile)

deploy:  ## Build Release and deploy plugins to the dedicated test profile (opt-in)
	$(DOTNET) build $(SLN) -c Release -p:Deploy=true

run-modded:  ## Start the server and launch Sailwind modded from the test profile
	$(call runscript,run-modded)

clean:  ## Remove bin, obj, target, and artifacts
ifeq ($(OS),Windows_NT)
	powershell -NoProfile -Command "Get-ChildItem -Path . -Include bin,obj -Recurse -Directory -Force -ErrorAction SilentlyContinue | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue; foreach ($$d in 'server/target','target','artifacts') { if (Test-Path $$d) { Remove-Item -Recurse -Force $$d } }"
else
	find . -type d \( -name bin -o -name obj \) -prune -exec rm -rf {} +
	rm -rf server/target target artifacts
endif
