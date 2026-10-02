// Read-only observations. No privilege adjustment, DACL writes or process control.
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text;
using Microsoft.Win32.SafeHandles;

public static class AircDaemonDiagnostics {
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern SafeFileHandle CreateFile(string name, uint access, uint share, IntPtr security, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern bool GetNamedPipeServerProcessId(SafeFileHandle pipe, out uint pid);
    [DllImport("kernel32.dll", SetLastError=true)]
    static extern IntPtr OpenProcess(uint access, bool inherit, uint pid);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    static extern bool QueryFullProcessImageName(IntPtr process, uint flags, StringBuilder text, ref uint size);
    [DllImport("advapi32.dll", SetLastError=true)]
    static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError=true)]
    static extern bool GetTokenInformation(IntPtr token, int kind, IntPtr buffer, int size, out int needed);
    [DllImport("kernel32.dll")]
    static extern bool CloseHandle(IntPtr handle);

    static void ReadToken(IntPtr token, int kind, string field, Dictionary<string, object> result) {
        int size = 4; // TOKEN_ELEVATION has a fixed DWORD payload.
        if (kind != 20) {
            GetTokenInformation(token, kind, IntPtr.Zero, 0, out size);
            int error = Marshal.GetLastWin32Error();
            if (size <= 0 || error != 122) { result[field + "Error"] = error; return; }
        }
        IntPtr buffer = Marshal.AllocHGlobal(size);
        try {
            if (!GetTokenInformation(token, kind, buffer, size, out size)) {
                result[field + "Error"] = Marshal.GetLastWin32Error(); return;
            }
            if (kind == 20) result[field] = Marshal.ReadInt32(buffer) != 0;
            else result[field] = new SecurityIdentifier(Marshal.ReadIntPtr(buffer)).Value;
        } finally { Marshal.FreeHGlobal(buffer); }
    }

    public static Dictionary<string, object> Inspect(string endpoint) {
        if (String.IsNullOrWhiteSpace(endpoint) || !endpoint.StartsWith(@"\\.\pipe\", StringComparison.OrdinalIgnoreCase))
            throw new ArgumentException("Expected the selected local named-pipe endpoint.");
        var result = new Dictionary<string, object> {
            {"endpoint", endpoint}, {"serverPid", "UNKNOWN"}, {"imagePath", "UNKNOWN"},
            {"tokenUserSid", "UNKNOWN"}, {"tokenElevated", "UNKNOWN"}, {"tokenIntegritySid", "UNKNOWN"},
            {"observerSid", WindowsIdentity.GetCurrent().User.Value}, {"observation", "UNKNOWN"}
        };
        // Match the protocol client's read/write access, but exchange no messages.
        // SECURITY_IDENTIFICATION prevents an endpoint from impersonating the observer.
        using (SafeFileHandle pipe = CreateFile(endpoint, 0xC0000000, 0, IntPtr.Zero, 3, 0x00110000, IntPtr.Zero)) {
            if (pipe.IsInvalid) { result["pipeOpenError"] = Marshal.GetLastWin32Error(); return result; }
            uint pid;
            if (!GetNamedPipeServerProcessId(pipe, out pid)) {
                result["serverPidError"] = Marshal.GetLastWin32Error(); return result;
            }
            result["serverPid"] = pid;
            result["observation"] = "ENDPOINT_SERVER_OBSERVED";
            // Keep the endpoint handle while querying its server. PID files and task
            // registrations are intentionally not treated as runtime evidence.
            IntPtr process = OpenProcess(0x1000, false, pid);
            if (process == IntPtr.Zero) { result["processOpenError"] = Marshal.GetLastWin32Error(); return result; }
            try {
                uint length = 32768;
                var path = new StringBuilder((int)length);
                if (QueryFullProcessImageName(process, 0, path, ref length)) result["imagePath"] = path.ToString();
                else result["imagePathError"] = Marshal.GetLastWin32Error();
                IntPtr token;
                if (!OpenProcessToken(process, 8, out token)) result["tokenOpenError"] = Marshal.GetLastWin32Error();
                else try {
                    ReadToken(token, 1, "tokenUserSid", result);
                    ReadToken(token, 20, "tokenElevated", result);
                    ReadToken(token, 25, "tokenIntegritySid", result);
                } finally { CloseHandle(token); }
            } finally { CloseHandle(process); }
        }
        return result;
    }
}
