// The seven numbered conformance checks. Each prints exactly one PASS/FAIL
// line and the run exits 0 only when every check passes. The harness is the
// deliverable: it exercises a real LiteNetLib client against a real Rust
// server over real FlatBuffers envelopes and a real SQLite database that
// survives a process restart.

using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Net;
using System.Net.Sockets;
using System.Threading;
using SwProto;

namespace Sailwind.ProtocolSmoke
{
    internal sealed class Checks
    {
        private const string TokenA = "smoke-token-A";
        private const string TokenB = "smoke-token-B";

        private readonly string _serverPath;
        private readonly string _configPath;
        private readonly string _workingDir;

        private uint _seq = 1;

        public Checks(string serverPath, string configPath, string workingDir)
        {
            _serverPath = serverPath;
            _configPath = configPath;
            _workingDir = workingDir;
        }

        private readonly struct Result
        {
            public Result(int number, string name, bool pass, string detail)
            {
                Number = number;
                Name = name;
                Pass = pass;
                Detail = detail;
            }

            public int Number { get; }

            public string Name { get; }

            public bool Pass { get; }

            public string Detail { get; }
        }

        public bool Run()
        {
            var results = new List<Result>();

            using var server = new ServerHarness(_serverPath, _configPath, _workingDir);
            IPEndPoint ep;
            try
            {
                server.Start(TimeSpan.FromSeconds(15));
                ep = server.Endpoint;
            }
            catch (Exception ex)
            {
                Console.WriteLine($"FAIL  0. server-start - {ex.Message}");
                return false;
            }

            Console.WriteLine($"info  server listening on {ep}");

            // Check 7 is set up first: spray malformed datagrams before any
            // scenario, then confirm at the end that the server survived.
            SprayHostileInput(ep, 100);

            var a = new SmokeClient("A");
            ulong playerIdA = 0;
            bool serverSurvived;
            try
            {
                a.Connect(ep.Address.ToString(), ep.Port);
                bool handshook = PumpUntil(() => a.Connected, 2000, a);
                results.Add(new Result(1, "handshake", handshook,
                    handshook ? "LiteNetLib Connect completed within 2s"
                              : "no PeerConnected within 2s (sw-net LiteNetLib framing not interoperating yet)"));

                ServerHello? helloA = handshook ? DoHello(a, "Smoke-A", TokenA) : null;
                bool helloOk = IsValidHello(helloA);
                playerIdA = helloA.HasValue ? helloA.Value.PlayerId : 0;
                results.Add(new Result(2, "hello", helloOk,
                    helloOk ? $"ServerHello accepted, player_id={playerIdA}, protocol_version={helloA.Value.Capabilities.Value.ProtocolVersion}"
                            : "no accepted ServerHello with player_id, capabilities, clock and weather"));

                results.Add(RunPresence(server, ep, a, playerIdA));
                results.Add(RunEcon(a));
                results.Add(RunMoorage(a));

                // Capture liveness after the spray and scenarios 1-5, before the
                // deliberate restart in check 6 tears the server down.
                serverSurvived = server.IsAlive;

                results.Add(RunRestartPersistence(server, a, playerIdA));
            }
            finally
            {
                a.Dispose();
            }

            results.Add(new Result(7, "hostile-input", serverSurvived,
                serverSurvived ? "server stayed alive through 100 malformed datagrams and checks 1-5"
                               : "server process exited during the spray or scenarios"));

            results.Sort((x, y) => x.Number.CompareTo(y.Number));
            var allPass = true;
            foreach (var r in results)
            {
                Console.WriteLine($"{(r.Pass ? "PASS" : "FAIL")}  {r.Number}. {r.Name} - {r.Detail}");
                allPass &= r.Pass;
            }

            var passed = results.FindAll(r => r.Pass).Count;
            Console.WriteLine($"info  {passed}/{results.Count} checks passed");
            return allPass;
        }

        // Check 3: presence and server-driven AoI. A second client B joins the
        // same cell while A streams 20 ClientState updates moving +X; B must see
        // A's cell added and A's x increasing.
        private Result RunPresence(ServerHarness server, IPEndPoint ep, SmokeClient a, ulong playerIdA)
        {
            try
            {
                if (!a.Connected || playerIdA == 0)
                {
                    return new Result(3, "presence-aoi", false, "skipped: A has no established session");
                }

                using var b = new SmokeClient("B");
                b.Connect(ep.Address.ToString(), ep.Port);
                if (!PumpUntil(() => b.Connected, 2000, a, b))
                {
                    return new Result(3, "presence-aoi", false, "client B failed to connect");
                }

                var helloB = DoHello(b, "Smoke-B", TokenB);
                if (!IsValidHello(helloB))
                {
                    return new Result(3, "presence-aoi", false, "client B failed the hello exchange");
                }

                bool sawAddedCell = false;
                var xs = new List<float>();

                for (var i = 0; i < 20; i++)
                {
                    float x = i * 2f;
                    a.Send(Codec.EncodeClientState(_seq++, x, 0f, 0f, 0f, 0f, 0f, 1f, 0f, 0f, 0f, 0, (uint)i));
                    var sw = Stopwatch.StartNew();
                    while (sw.ElapsedMilliseconds < 50)
                    {
                        a.Poll();
                        b.Poll();
                        DrainPresence(b, playerIdA, ref sawAddedCell, xs);
                        Thread.Sleep(2);
                    }
                }

                var tail = Stopwatch.StartNew();
                while (tail.ElapsedMilliseconds < 500)
                {
                    a.Poll();
                    b.Poll();
                    DrainPresence(b, playerIdA, ref sawAddedCell, xs);
                    Thread.Sleep(5);
                }

                bool increasing = IsIncreasing(xs);
                bool ok = sawAddedCell && increasing;
                var detail = ok
                    ? $"B saw A's cell added and {xs.Count} monotonically increasing x samples"
                    : $"sawAddedCell={sawAddedCell}, x-samples={xs.Count}, increasing={increasing}";
                return new Result(3, "presence-aoi", ok, detail);
            }
            catch (Exception ex)
            {
                return new Result(3, "presence-aoi", false, $"exception: {ex.Message}");
            }
        }

        // Check 4: economy idempotency. txn_id 1 credits +100 once; a replay of
        // the same txn_id leaves the balance at 100.
        private Result RunEcon(SmokeClient a)
        {
            try
            {
                if (!a.Connected)
                {
                    return new Result(4, "econ-idempotency", false, "skipped: A has no connection");
                }

                var txn = Codec.EncodeEconTxn(_seq++, 1, 100, 0, "smoke");
                var ack1 = SendAndWait(a, txn, e => e.PayloadType == Payload.LedgerAck, 3000, 250);
                if (!ack1.HasValue)
                {
                    return new Result(4, "econ-idempotency", false, "no LedgerAck for txn 1");
                }

                var la1 = ack1.Value.PayloadAsLedgerAck();
                if (la1.NewBalance != 100)
                {
                    return new Result(4, "econ-idempotency", false, $"new_balance {la1.NewBalance} != 100 after first credit");
                }

                var replay = Codec.EncodeEconTxn(_seq++, 1, 100, 0, "smoke");
                var ack2 = SendAndWait(a, replay, e => e.PayloadType == Payload.LedgerAck, 3000, 250);
                if (!ack2.HasValue)
                {
                    return new Result(4, "econ-idempotency", false, "no LedgerAck on replay of txn 1");
                }

                var la2 = ack2.Value.PayloadAsLedgerAck();
                bool ok = la2.NewBalance == 100;
                return new Result(4, "econ-idempotency", ok,
                    ok ? "balance 100 after credit and unchanged on replay"
                       : $"idempotency broken: replay produced balance {la2.NewBalance}");
            }
            catch (Exception ex)
            {
                return new Result(4, "econ-idempotency", false, $"exception: {ex.Message}");
            }
        }

        // Check 5: moorage. A MoorRequest is accepted and returns a record.
        private Result RunMoorage(SmokeClient a)
        {
            try
            {
                if (!a.Connected)
                {
                    return new Result(5, "moorage-ack", false, "skipped: A has no connection");
                }

                var req = Codec.EncodeMoorRequest(_seq++, 10f, 0f, 10f, 0f, 0f, 0f, 1f, "Smoke Harbor");
                var env = SendAndWait(a, req, e => e.PayloadType == Payload.MoorAck, 3000, 250);
                if (!env.HasValue)
                {
                    return new Result(5, "moorage-ack", false, "no MoorAck received");
                }

                var ack = env.Value.PayloadAsMoorAck();
                bool ok = ack.Accepted && ack.Record.HasValue;
                return new Result(5, "moorage-ack", ok,
                    ok ? $"moored boat_id={ack.Record.Value.BoatId}"
                       : $"MoorAck.accepted={ack.Accepted}, hasRecord={ack.Record.HasValue}");
            }
            catch (Exception ex)
            {
                return new Result(5, "moorage-ack", false, $"exception: {ex.Message}");
            }
        }

        // Check 6: kill/restart persistence. After a hard restart on the same db,
        // reconnecting A with the same token recovers the 100-gold balance and
        // the moorage record in the joined cell's snapshot.
        private Result RunRestartPersistence(ServerHarness server, SmokeClient a, ulong playerIdA)
        {
            try
            {
                a.Disconnect();

                server.Kill();
                server.Start(TimeSpan.FromSeconds(15));
                var ep = server.Endpoint;

                using var a2 = new SmokeClient("A2");
                a2.Connect(ep.Address.ToString(), ep.Port);
                if (!PumpUntil(() => a2.Connected, 2000, a2))
                {
                    return new Result(6, "restart-persistence", false, "reconnect after restart failed");
                }

                var hello = DoHello(a2, "Smoke-A", TokenA);
                if (!IsValidHello(hello))
                {
                    return new Result(6, "restart-persistence", false, "hello after restart failed");
                }

                if (hello.Value.BalanceGold != 100)
                {
                    return new Result(6, "restart-persistence", false,
                        $"balance_gold {hello.Value.BalanceGold} != 100 after restart (not persisted)");
                }

                bool foundMooring = false;
                var tail = Stopwatch.StartNew();
                while (tail.ElapsedMilliseconds < 1500 && !foundMooring)
                {
                    // Nudge the server into emitting the joined cell's snapshot.
                    a2.Send(Codec.EncodeClientState(_seq++, 0f, 0f, 0f, 0f, 0f, 0f, 1f, 0f, 0f, 0f, 0, 0));
                    var window = Stopwatch.StartNew();
                    while (window.ElapsedMilliseconds < 150 && !foundMooring)
                    {
                        a2.Poll();
                        while (a2.TryReceive(out var data))
                        {
                            var env = Codec.TryDecodeEnvelope(data);
                            if (env.HasValue && env.Value.PayloadType == Payload.CellSnapshot)
                            {
                                var cs = env.Value.PayloadAsCellSnapshot();
                                if (cs.MooringsLength > 0)
                                {
                                    foundMooring = true;
                                }
                            }
                        }

                        Thread.Sleep(5);
                    }
                }

                return new Result(6, "restart-persistence", foundMooring,
                    foundMooring ? "balance_gold 100 restored and moorage record present in joined cell"
                                 : "balance restored but no moorage record found in the joined cell snapshot");
            }
            catch (Exception ex)
            {
                return new Result(6, "restart-persistence", false, $"exception: {ex.Message}");
            }
        }

        private void DrainPresence(SmokeClient b, ulong playerIdA, ref bool sawAddedCell, List<float> xs)
        {
            while (b.TryReceive(out var data))
            {
                var env = Codec.TryDecodeEnvelope(data);
                if (!env.HasValue)
                {
                    continue;
                }

                switch (env.Value.PayloadType)
                {
                    case Payload.AoiUpdate:
                        var aoi = env.Value.PayloadAsAoiUpdate();
                        for (var j = 0; j < aoi.AddedLength; j++)
                        {
                            var cell = aoi.Added(j);
                            if (cell.HasValue && cell.Value.Cx == 0 && cell.Value.Cz == 0)
                            {
                                sawAddedCell = true;
                            }
                        }

                        break;
                    case Payload.SnapshotDelta:
                        var snap = env.Value.PayloadAsSnapshotDelta();
                        for (var j = 0; j < snap.PlayersLength; j++)
                        {
                            var ps = snap.Players(j);
                            if (ps.HasValue && ps.Value.PlayerId == playerIdA && ps.Value.Pos.HasValue)
                            {
                                xs.Add(ps.Value.Pos.Value.X);
                            }
                        }

                        break;
                    case Payload.CellSnapshot:
                        var cell2 = env.Value.PayloadAsCellSnapshot();
                        if (cell2.Cell.HasValue && cell2.Cell.Value.Cx == 0 && cell2.Cell.Value.Cz == 0)
                        {
                            sawAddedCell = true;
                        }

                        for (var j = 0; j < cell2.PlayersLength; j++)
                        {
                            var ps = cell2.Players(j);
                            if (ps.HasValue && ps.Value.PlayerId == playerIdA && ps.Value.Pos.HasValue)
                            {
                                xs.Add(ps.Value.Pos.Value.X);
                            }
                        }

                        break;
                }
            }
        }

        private ServerHello? DoHello(SmokeClient c, string name, string token)
        {
            var hello = Codec.EncodeClientHello(_seq++, name, token, "smoke", "0.0.0", string.Empty);
            var env = SendAndWait(c, hello, e => e.PayloadType == Payload.ServerHello, 6000, 250);
            return env.HasValue ? env.Value.PayloadAsServerHello() : (ServerHello?)null;
        }

        // Resends `payload` every retryMs (safe because hello, econ and moorage
        // are server-idempotent) and pumps events until a matching envelope
        // arrives or the timeout elapses.
        private static Envelope? SendAndWait(SmokeClient c, byte[] payload, Func<Envelope, bool> match, int timeoutMs, int retryMs)
        {
            var sw = Stopwatch.StartNew();
            long lastSend = -100000;
            while (sw.ElapsedMilliseconds < timeoutMs)
            {
                if (sw.ElapsedMilliseconds - lastSend >= retryMs)
                {
                    c.Send(payload);
                    lastSend = sw.ElapsedMilliseconds;
                }

                c.Poll();
                while (c.TryReceive(out var data))
                {
                    var env = Codec.TryDecodeEnvelope(data);
                    if (env.HasValue && match(env.Value))
                    {
                        return env.Value;
                    }
                }

                Thread.Sleep(5);
            }

            return null;
        }

        private static bool PumpUntil(Func<bool> condition, int timeoutMs, params SmokeClient[] clients)
        {
            var sw = Stopwatch.StartNew();
            while (sw.ElapsedMilliseconds < timeoutMs)
            {
                foreach (var c in clients)
                {
                    c.Poll();
                }

                if (condition())
                {
                    return true;
                }

                Thread.Sleep(5);
            }

            foreach (var c in clients)
            {
                c.Poll();
            }

            return condition();
        }

        private static bool IsValidHello(ServerHello? hello)
        {
            return hello.HasValue
                   && hello.Value.Accepted
                   && hello.Value.PlayerId != 0
                   && hello.Value.Capabilities.HasValue
                   && hello.Value.Capabilities.Value.ProtocolVersion == Codec.ProtocolVersion
                   && hello.Value.Clock.HasValue
                   && hello.Value.Weather.HasValue;
        }

        private static bool IsIncreasing(List<float> xs)
        {
            if (xs.Count < 2)
            {
                return false;
            }

            for (var i = 1; i < xs.Count; i++)
            {
                if (xs[i] < xs[i - 1])
                {
                    return false;
                }
            }

            return xs[xs.Count - 1] > xs[0];
        }

        private static void SprayHostileInput(IPEndPoint endpoint, int count)
        {
            using var udp = new UdpClient();
            var rng = new Random(0xC0FFEE);
            for (var i = 0; i < count; i++)
            {
                var buf = new byte[rng.Next(1, 64)];
                rng.NextBytes(buf);
                try
                {
                    udp.Send(buf, buf.Length, endpoint);
                }
                catch (SocketException)
                {
                    // A dropped malformed datagram is itself a valid outcome.
                }
            }
        }
    }
}
