using System;
using BepInEx;
using BepInEx.Configuration;
using Sailwind.Api;
using Sailwind.Online.Client.Net;
using Sailwind.Online.Client.Runtime;
using Sailwind.Online.Client.Sync;
using Sailwind.Online.Client.UI;
using UnityEngine;

namespace Sailwind.Online.Client
{
    /// <summary>
    /// Sailwind Online MMO client. Hard-depends on Sailwind.API for all game reads.
    /// Boots the network client once the API signals the world is loaded and its surface verified,
    /// then reports the local boat pose and decodes server snapshots each frame.
    /// </summary>
    [BepInPlugin(PluginGuid, PluginName, PluginVersion)]
    [BepInDependency(ApiGuid, BepInDependency.DependencyFlags.HardDependency)]
    public sealed class Plugin : BaseUnityPlugin
    {
        public const string PluginGuid = "com.aramdevdocs.sailwind.online";
        public const string PluginName = "Sailwind Online";
        public const string PluginVersion = "0.1.0";
        private const string ApiGuid = "com.aramdevdocs.sailwind.api";

        private ConfigEntry<string> _host;
        private ConfigEntry<int> _port;
        private ConfigEntry<string> _displayName;
        private ConfigEntry<string> _token;

        private MainThreadDispatcher _dispatcher;
        private NetClient _net;
        private StateReporter _reporter;
        private StatusHud _hud;
        private bool _apiReady;

        private void Awake()
        {
            _host = Config.Bind("Server", "Host", "127.0.0.1", "Address of the Sailwind Online server.");
            _port = Config.Bind("Server", "Port", 38455, "UDP port of the Sailwind Online server.");
            _displayName = Config.Bind("Player", "DisplayName", "Sailor", "Name shown to other players.");
            _token = Config.Bind("Player", "Token", string.Empty, "Identity token presented to the server.");

            _dispatcher = new MainThreadDispatcher();
            _net = new NetClient(new BepInExNetLog(Logger));
            _reporter = new StateReporter(_net);
            _hud = new StatusHud(_net);

            SailwindApi.Ready += OnApiReady;

            Logger.LogInfo(PluginName + " " + PluginVersion + " loaded; waiting for Sailwind.API.");
        }

        private void OnApiReady()
        {
            _apiReady = true;

            ConnectOptions options = new ConnectOptions
            {
                Host = _host.Value,
                Port = _port.Value,
                DisplayName = _displayName.Value,
                Token = _token.Value,
                GameBuild = Application.version,
                ModVersion = PluginVersion,
                ApiSurfaceHash = SailwindApi.SurfaceHash
            };

            _net.Connect(options);
        }

        private void Update()
        {
            _dispatcher.Drain(OnDispatchError);

            if (_net != null)
            {
                _net.Poll();
            }

            if (_apiReady && _reporter != null)
            {
                _reporter.Tick();
            }
        }

        private void OnGUI()
        {
            if (_hud != null)
            {
                _hud.Draw();
            }
        }

        private void OnDestroy()
        {
            SailwindApi.Ready -= OnApiReady;

            if (_net != null)
            {
                _net.Dispose();
            }
        }

        private void OnDispatchError(Exception ex)
        {
            Logger.LogError("Queued main-thread work threw: " + ex);
        }
    }
}
