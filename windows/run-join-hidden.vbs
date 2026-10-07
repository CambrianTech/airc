' Windowless Task Scheduler entry point. PowerShell supervises AIRC without
' a console and returns the join client's exit code.
Set shell = CreateObject("WScript.Shell")
scriptDir = CreateObject("Scripting.FileSystemObject").GetParentFolderName(WScript.ScriptFullName)
powershell = shell.ExpandEnvironmentStrings("%SystemRoot%") & "\System32\WindowsPowerShell\v1.0\powershell.exe"
command = Chr(34) & powershell & Chr(34) & " -NoProfile -NonInteractive -ExecutionPolicy RemoteSigned -File " & _
    Chr(34) & scriptDir & "\airc-join-hidden.ps1" & Chr(34) & " -AircPath " & _
    Chr(34) & WScript.Arguments(0) & Chr(34) & " -LogDirectory " & _
    Chr(34) & WScript.Arguments(1) & Chr(34)
WScript.Quit shell.Run(command, 0, True)
