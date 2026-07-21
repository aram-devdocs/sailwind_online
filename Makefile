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

.PHONY: help setup codegen contracts check audit build build-ci test \
        server-build server-run smoke deploy clean

help:  ## List available targets
	@echo Sailwind Online - make targets
	@echo.
	@echo   setup         Provision lib/ and the sandbox from a local Steam install
	@echo   codegen       Regenerate the Sailwind.API surface from the game assembly
	@echo   contracts     Regenerate FlatBuffers C# and Rust from contracts/fbs
	@echo   check         Fast gate: game-free build plus tests plus Rust checks
	@echo   audit         Full gate: check plus the game-coupled tier when lib/ is present
	@echo   build         Build the full solution in Release (needs lib/)
	@echo   build-ci      Build the game-free solution filter in Release
	@echo   test          Game-free dotnet tests plus cargo test
	@echo   server-build  Build the Rust server workspace
	@echo   server-run    Run the Rust server (sailwind-online-server)
	@echo   smoke         Build the server in Release and run the protocol-smoke harness
	@echo   deploy        Build Release; Deploy.targets copies plugins to the profile
	@echo   clean         Remove bin, obj, target, and artifacts

setup:  ## Provision lib/ and the sandbox from a local Steam install
	$(call runscript,setup-game)

codegen:  ## Regenerate the Sailwind.API surface from the game assembly
	$(call runscript,generate-api)

contracts:  ## Regenerate FlatBuffers C# and Rust from contracts/fbs
	$(call runscript,gen-contracts)

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

server-build:  ## Build the Rust server workspace
	$(CARGO) build --manifest-path $(SERVER_MANIFEST)

server-run:  ## Run the Rust server
	$(CARGO) run --manifest-path $(SERVER_MANIFEST) -p sailwind-online-server

smoke:  ## Build the server in Release and run the protocol-smoke harness
	$(CARGO) build --release --manifest-path $(SERVER_MANIFEST)
	$(DOTNET) run --project tools/protocol-smoke

deploy:  ## Build Release; Deploy.targets copies plugins to the profile
	$(DOTNET) build $(SLN) -c Release

clean:  ## Remove bin, obj, target, and artifacts
ifeq ($(OS),Windows_NT)
	powershell -NoProfile -Command "Get-ChildItem -Path . -Include bin,obj -Recurse -Directory -Force -ErrorAction SilentlyContinue | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue; foreach ($$d in 'server/target','target','artifacts') { if (Test-Path $$d) { Remove-Item -Recurse -Force $$d } }"
else
	find . -type d \( -name bin -o -name obj \) -prune -exec rm -rf {} +
	rm -rf server/target target artifacts
endif
