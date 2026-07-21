# Sailwind Online

Persistent shared seas for Sailwind. Connect to a Sailwind Online server and
share the world with other sailors: see their boats, moor your own in a living
harbor that stays put while you are away, and trade in a shared, server-checked
economy. The world clock and weather come from the server, so everyone sails
under one sky.

Your game still runs the sailing simulation locally; the server holds thin
authority over the shared state (presence, moorage, the gold ledger, and the
clock and weather seed).

## Requirements

- A running Sailwind Online server to connect to (host your own or join one).
- The Sailwind API dependency, installed automatically with this package.

## Getting started

Install through your mod manager, launch the game, and set the server host and
port plus your display name in the plugin config. The in-game HUD shows the
connection state and the shared world time.

## Notes

- The Thunderstore team namespace `aram_devdocs` is a placeholder to register.
- This is an early release: remote boats appear as position updates, with full
  in-world ghost rendering and interpolation arriving in a later milestone.
