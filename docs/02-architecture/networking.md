# Networking

The client and server speak UDP with FlatBuffers message bodies. The client
plugin uses the LiteNetLib transport; the Rust server implements the same wire
framing so a real LiteNetLib client interoperates with it. Every message is a
FlatBuffers `Envelope` carrying a sequence number and a payload union, so one
decode path handles all message types.

## Transport

- Datagram UDP with a connect handshake and a connect key, so the server can
  reject unrelated traffic before it reaches the message layer.
- The client polls the transport on the game's main thread, so message handlers
  already run on the Unity thread and need no cross-thread marshalling at
  init-0.
- The server runs a single-threaded fixed-tick loop over a nonblocking socket:
  a net pump each tick and a snapshot broadcast at a slower rate. A thin
  authority with synchronous persistence wants a deterministic tick, and the
  decision is reversible behind the transport's event API.

## LiteNetLib version decision

The client pins LiteNetLib to the 1.3.x line (1.3.1), and the Rust framing and
the protocol conformance harness both target that same wire format. The reason
is compatibility with the game's runtime: the 2.x line dropped the older .NET
Framework and netstandard 2.0 targets the game's Mono runtime needs, while the
1.3.x line still ships them. The wire format is the contract, so all three
sides (client, server framing, conformance harness) MUST agree on one line.

## Reliability scope at init-0

The transport ships unreliable-only at init-0. There is no reliable-ordered
channel yet, so every send is unreliable, because sending reliable-ordered
traffic at a server that cannot acknowledge it would retransmit forever.

Messages that must arrive (the hello handshake, economy transactions, moorage
requests) get their reliability from the application, not the transport: the
sender retries on a fixed interval until it sees the reply, and the server is
idempotent so a duplicate is harmless. Economy transactions carry a transaction
id and moorage state is keyed, so replaying either one converges instead of
double-applying. A reliable-ordered channel is a later milestone.

## Message model

- One `Envelope` table wraps every message, with a sequence number and a
  payload union. The protocol version is a constant defined in the schema and
  read from generated code on both sides, so there is one source of truth for
  the version number.
- Client-to-server carries the client's own state and its intent (hello, state
  updates, transaction and moorage requests, chat).
- Server-to-client carries acknowledgements, snapshot deltas, and area-of-
  interest updates.

## Interest management and snapshots

The server drives interest management: it derives a grid cell from each client's
reported position and subscribes that client to a block of nearby cells,
sending add and remove updates as the client crosses cell boundaries. Clients
never subscribe themselves, so bandwidth per client stays bounded as the
population grows.

Remote entities arrive as timestamped snapshot deltas. The client keeps a ring
buffer per entity shaped for interpolation between snapshots, with an
extrapolation cap for gaps. At init-0 the client decodes and caches snapshots
but does not yet render remote entities in the world; interpolation rendering is
a later milestone.
