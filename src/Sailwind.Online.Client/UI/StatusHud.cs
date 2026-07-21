using Sailwind.Online.Client.Net;
using UnityEngine;

namespace Sailwind.Online.Client.UI
{
    /// <summary>
    /// Zero-asset IMGUI overlay: one line summarising the online session. Drawn from OnGUI on the
    /// plugin. Real in-world UI (chat, moorage) is a later milestone; this is a diagnostics line.
    /// </summary>
    public sealed class StatusHud
    {
        private readonly NetClient _net;
        private GUIStyle _style;

        public StatusHud(NetClient net)
        {
            _net = net;
        }

        /// <summary>Call from OnGUI.</summary>
        public void Draw()
        {
            if (_style == null)
            {
                _style = new GUIStyle(GUI.skin.label)
                {
                    fontSize = 12,
                    alignment = TextAnchor.UpperLeft
                };
                _style.normal.textColor = Color.white;
            }

            int ping = _net.Ping;
            string pingText = ping >= 0 ? ping + "ms" : "--";

            string line =
                "[SW Online] " + _net.StatusText +
                " | ping " + pingText +
                " | players " + _net.Cache.PlayerCount +
                " boats " + _net.Cache.BoatCount +
                " | day " + _net.ServerDay +
                " " + NetClient.FormatTimeOfDay(_net.ServerTimeOfDay);

            GUI.Label(new Rect(10f, 10f, 960f, 22f), line, _style);
        }
    }
}
