// Spawns and supervises the Rust sw-server binary for the conformance run.
//
// The harness owns the server lifecycle: it writes a config.toml into a temp
// directory (so the SQLite db path is stable across a kill/restart, which the
// persistence check depends on), starts the process, and blocks until the
// server prints its readiness line ("listening on <addr>") on stdout. The
// endpoint to connect to is parsed from that line rather than assumed.

using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.IO;
using System.Net;
using System.Text;
using System.Threading;

namespace Sailwind.ProtocolSmoke
{
    internal sealed class ServerHarness : IDisposable
    {
        private const string ReadinessMarker = "listening on";

        private readonly string _serverPath;
        private readonly string _configPath;
        private readonly string _workingDir;
        private readonly List<string> _log = new List<string>();
        private readonly object _logLock = new object();

        private Process _process;
        private ManualResetEventSlim _ready;
        private IPEndPoint _endpoint;

        public ServerHarness(string serverPath, string configPath, string workingDir)
        {
            _serverPath = serverPath;
            _configPath = configPath;
            _workingDir = workingDir;
        }

        public IPEndPoint Endpoint => _endpoint;

        public bool IsAlive => _process != null && !_process.HasExited;

        public void Start(TimeSpan timeout)
        {
            _ready = new ManualResetEventSlim(false);
            _endpoint = null;

            var psi = new ProcessStartInfo
            {
                FileName = _serverPath,
                WorkingDirectory = _workingDir,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                UseShellExecute = false,
                CreateNoWindow = true,
            };
            // Offer the config three ways so we match whatever CLI/env contract
            // sw-server settles on: positional flag, environment variable, and a
            // config.toml in the working directory.
            psi.ArgumentList.Add("--config");
            psi.ArgumentList.Add(_configPath);
            psi.Environment["SW_CONFIG"] = _configPath;

            _process = new Process { StartInfo = psi, EnableRaisingEvents = true };
            _process.OutputDataReceived += (s, e) => OnLine(e.Data);
            _process.ErrorDataReceived += (s, e) => OnLine(e.Data);

            if (!_process.Start())
            {
                throw new InvalidOperationException($"failed to start server process: {_serverPath}");
            }

            _process.BeginOutputReadLine();
            _process.BeginErrorReadLine();

            if (!_ready.Wait(timeout))
            {
                var tail = LogTail(20);
                Kill();
                throw new TimeoutException(
                    $"server did not print '{ReadinessMarker} <addr>' within {timeout.TotalSeconds:0}s. Last output:\n{tail}");
            }

            if (_endpoint == null)
            {
                var tail = LogTail(20);
                Kill();
                throw new InvalidOperationException($"server signalled readiness but no endpoint was parsed. Last output:\n{tail}");
            }
        }

        public void Kill()
        {
            var proc = _process;
            if (proc == null)
            {
                return;
            }

            try
            {
                if (!proc.HasExited)
                {
                    proc.Kill(entireProcessTree: true);
                    proc.WaitForExit(5000);
                }
            }
            catch (InvalidOperationException)
            {
                // Process already gone; nothing to kill.
            }
            finally
            {
                proc.Dispose();
                _process = null;
            }
        }

        public string LogTail(int lines)
        {
            lock (_logLock)
            {
                var start = Math.Max(0, _log.Count - lines);
                var sb = new StringBuilder();
                for (var i = start; i < _log.Count; i++)
                {
                    sb.Append("    | ").AppendLine(_log[i]);
                }

                return sb.ToString();
            }
        }

        public void Dispose()
        {
            Kill();
        }

        private void OnLine(string line)
        {
            if (line == null)
            {
                return;
            }

            lock (_logLock)
            {
                _log.Add(line);
            }

            var idx = line.IndexOf(ReadinessMarker, StringComparison.OrdinalIgnoreCase);
            if (idx < 0)
            {
                return;
            }

            var rest = line.Substring(idx + ReadinessMarker.Length).Trim();
            if (TryParseEndpoint(rest, out var ep))
            {
                _endpoint = ep;
                _ready?.Set();
            }
        }

        // Parses the first "host:port" token from the readiness line. A wildcard
        // bind (0.0.0.0 / [::]) is rewritten to loopback so the client can reach
        // it on the same host.
        private static bool TryParseEndpoint(string text, out IPEndPoint endpoint)
        {
            endpoint = null;
            if (string.IsNullOrWhiteSpace(text))
            {
                return false;
            }

            var token = text.Split(new[] { ' ', '\t', ',' }, StringSplitOptions.RemoveEmptyEntries)[0];
            var colon = token.LastIndexOf(':');
            if (colon <= 0 || colon >= token.Length - 1)
            {
                return false;
            }

            var hostPart = token.Substring(0, colon).Trim('[', ']');
            var portPart = token.Substring(colon + 1);
            if (!int.TryParse(portPart, NumberStyles.Integer, CultureInfo.InvariantCulture, out var port))
            {
                return false;
            }

            if (!IPAddress.TryParse(hostPart, out var addr))
            {
                return false;
            }

            if (addr.Equals(IPAddress.Any) || addr.Equals(IPAddress.IPv6Any))
            {
                addr = IPAddress.Loopback;
            }

            endpoint = new IPEndPoint(addr, port);
            return true;
        }
    }
}
