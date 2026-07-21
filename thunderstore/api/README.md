# Sailwind API

A stable, version-checked modding API for Sailwind. Sailwind.API sits between
other mods and the game's internals: it exposes generated interfaces and events
over confirmed game types (the world clock, wind, the player boat, save
lifecycle) so mods build against one dependable surface instead of reaching
into `Assembly-CSharp` by hand.

The API carries a hash of the game surface it was built against and checks it at
startup. When a game update moves something, the affected feature reports itself
unavailable and logs a clear message, rather than crashing with a Harmony
exception mid-session.

## For mod authors

Declare a hard dependency on the plugin GUID `com.aramdevdocs.sailwind.api` and
read the game through the API surface. Reaching past the API into game internals
gives up the version-safety this package exists to provide.

## Notes

- This package complements SailwindModdingHelper; it does not replace it and
  does not break its consumers.
- The Thunderstore team namespace `aram_devdocs` is a placeholder to register.
