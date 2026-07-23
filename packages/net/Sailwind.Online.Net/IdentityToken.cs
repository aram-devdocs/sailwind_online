using System;
using System.Security.Cryptography;

namespace Sailwind.Online.Client.Net
{
    public static class IdentityToken
    {
        public static string GetOrCreate(string configuredToken, Action<string> persist)
        {
            if (!string.IsNullOrEmpty(configuredToken))
            {
                return configuredToken;
            }

            byte[] bytes = new byte[32];
            using (RandomNumberGenerator random = RandomNumberGenerator.Create())
            {
                random.GetBytes(bytes);
            }

            string token = Convert.ToBase64String(bytes)
                .TrimEnd('=')
                .Replace('+', '-')
                .Replace('/', '_');

            persist(token);
            return token;
        }
    }
}
