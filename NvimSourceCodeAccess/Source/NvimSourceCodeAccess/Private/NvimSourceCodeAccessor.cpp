#include "NvimSourceCodeAccessor.h"

#include "Dom/JsonObject.h"
#include "HAL/PlatformProcess.h"
#include "HAL/FileManager.h"
#include "Interfaces/IPluginManager.h"
#include "JsonObjectConverter.h"
#include "Misc/FileHelper.h"
#include "Misc/Paths.h"
#include "Serialization/JsonSerializer.h"
#include "Serialization/JsonWriter.h"

#define LOCTEXT_NAMESPACE "NvimSourceCodeAccessor"

namespace NvimSourceCodeAccess
{
	static const FName AccessorName(TEXT("Nvim"));
}

void FNvimSourceCodeAccessor::Startup()
{
	RefreshAvailability();
}

void FNvimSourceCodeAccessor::Shutdown()
{
}

void FNvimSourceCodeAccessor::RefreshAvailability()
{
	Location = FExecutableLocation{};
	Remote = FRemoteLocation{};
	RemoteMode = ResolveRemoteMode();
	if (RemoteMode != ERemoteMode::Never)
	{
		ResolveRemoteLocation(Remote);
	}
	ResolveExecutableLocation(Location);
}

bool FNvimSourceCodeAccessor::CanAccessSourceCode() const
{
	switch (RemoteMode)
	{
	case ERemoteMode::Only:
		return Remote.IsValid();
	case ERemoteMode::Never:
		return Location.IsValid();
	case ERemoteMode::Auto:
	case ERemoteMode::Prefer:
	default:
		return Remote.IsValid() || Location.IsValid();
	}
}

FName FNvimSourceCodeAccessor::GetFName() const
{
	return NvimSourceCodeAccess::AccessorName;
}

FText FNvimSourceCodeAccessor::GetNameText() const
{
	return LOCTEXT("NvimDisplayName", "Neovim");
}

FText FNvimSourceCodeAccessor::GetDescriptionText() const
{
	return LOCTEXT("NvimDisplayDescription", "Open source code files in Neovim.");
}

bool FNvimSourceCodeAccessor::OpenSolution()
{
	return OpenSolutionAtPath(FPaths::ProjectDir());
}

bool FNvimSourceCodeAccessor::OpenSolutionAtPath(const FString& InSolutionPath)
{
	FString TargetPath = NormalizeTargetPath(InSolutionPath);
	if (TargetPath.IsEmpty())
	{
		TargetPath = FPaths::ProjectDir();
	}

	TSharedRef<FJsonObject> RequestObject = MakeShared<FJsonObject>();
	RequestObject->SetStringField(TEXT("kind"), TEXT("activate_project"));
	RequestObject->SetStringField(TEXT("project_root"), TargetPath);
	RequestObject->SetBoolField(TEXT("boot"), true);
	if (LaunchBridgeRequest(RequestObject, TargetPath))
	{
		return true;
	}

	if (ShouldUseRemote())
	{
		return LaunchRemote({ TargetPath }, true);
	}

	if (!Location.IsValid() || !AllowsLocalFallback())
	{
		return false;
	}

	return Launch({ TargetPath });
}

bool FNvimSourceCodeAccessor::DoesSolutionExist() const
{
	return FPaths::DirectoryExists(FPaths::ProjectDir());
}

bool FNvimSourceCodeAccessor::OpenFileAtLine(const FString& FullPath, int32 LineNumber, int32 ColumnNumber)
{
	FString ResolvedPath;
	if (!ResolveSourcePath(FullPath, ResolvedPath))
	{
		return false;
	}

	LineNumber = LineNumber > 0 ? LineNumber : 1;
	ColumnNumber = ColumnNumber > 0 ? ColumnNumber : 1;

	TSharedRef<FJsonObject> RequestObject = MakeShared<FJsonObject>();
	RequestObject->SetStringField(TEXT("kind"), TEXT("open"));
	RequestObject->SetStringField(TEXT("project_root"), NormalizeTargetPath(FPaths::ProjectDir()));
	RequestObject->SetStringField(TEXT("file_path"), ResolvedPath);
	RequestObject->SetNumberField(TEXT("line"), LineNumber);
	RequestObject->SetNumberField(TEXT("col"), ColumnNumber > 0 ? ColumnNumber - 1 : 0);
	RequestObject->SetBoolField(TEXT("boot"), true);
	if (LaunchBridgeRequest(RequestObject, FPaths::ProjectDir()))
	{
		return true;
	}

	TArray<FString> Args;
	Args.Add(FString::Printf(TEXT("+call cursor(%d,%d)"), LineNumber, ColumnNumber));
	Args.Add(ResolvedPath);

	if (ShouldUseRemote())
	{
		return LaunchRemote(Args);
	}

	if (!Location.IsValid() || !AllowsLocalFallback())
	{
		return false;
	}

	return Launch(Args);
}

bool FNvimSourceCodeAccessor::OpenSourceFiles(const TArray<FString>& AbsoluteSourcePaths)
{
	if (AbsoluteSourcePaths.IsEmpty())
	{
		return false;
	}

	TArray<FString> ResolvedPaths;
	ResolvedPaths.Reserve(AbsoluteSourcePaths.Num());

	for (const FString& SourcePath : AbsoluteSourcePaths)
	{
		FString ResolvedPath;
		if (ResolveSourcePath(SourcePath, ResolvedPath))
		{
			ResolvedPaths.Add(ResolvedPath);
		}
	}

	if (ResolvedPaths.Num() == 0)
	{
		return false;
	}

	TSharedRef<FJsonObject> RequestObject = MakeShared<FJsonObject>();
	RequestObject->SetStringField(TEXT("kind"), TEXT("open_files"));
	RequestObject->SetStringField(TEXT("project_root"), NormalizeTargetPath(FPaths::ProjectDir()));
	RequestObject->SetBoolField(TEXT("boot"), true);

	TArray<TSharedPtr<FJsonValue>> JsonPaths;
	JsonPaths.Reserve(ResolvedPaths.Num());
	for (const FString& Path : ResolvedPaths)
	{
		JsonPaths.Add(MakeShared<FJsonValueString>(Path));
	}
	RequestObject->SetArrayField(TEXT("files"), JsonPaths);

	if (LaunchBridgeRequest(RequestObject, FPaths::ProjectDir()))
	{
		return true;
	}

	if (ShouldUseRemote())
	{
		return LaunchRemote(ResolvedPaths);
	}

	if (!Location.IsValid() || !AllowsLocalFallback())
	{
		return false;
	}

	return Launch(ResolvedPaths);
}

bool FNvimSourceCodeAccessor::AddSourceFiles(const TArray<FString>& AbsoluteSourcePaths, const TArray<FString>& AvailableModules)
{
	return true;
}

bool FNvimSourceCodeAccessor::SaveAllOpenDocuments() const
{
	return true;
}

void FNvimSourceCodeAccessor::Tick(const float DeltaTime)
{
}

FString FNvimSourceCodeAccessor::QuoteArg(const FString& Value)
{
	FString Escaped = Value.Replace(TEXT("\""), TEXT("\\\""));
	return FString::Printf(TEXT("\"%s\""), *Escaped);
}

FString FNvimSourceCodeAccessor::QuotePowerShellLiteral(const FString& Value)
{
	FString Escaped = Value.Replace(TEXT("'"), TEXT("''"));
	return FString::Printf(TEXT("'%s'"), *Escaped);
}

FString FNvimSourceCodeAccessor::QuoteVimLiteral(const FString& Value)
{
	FString Escaped = Value.Replace(TEXT("'"), TEXT("''"));
	return FString::Printf(TEXT("'%s'"), *Escaped);
}

FString FNvimSourceCodeAccessor::NormalizeExecutablePath(FString Value)
{
	Value = Value.TrimStartAndEnd();
	Value.RemoveFromStart(TEXT("\""));
	Value.RemoveFromEnd(TEXT("\""));
	return Value;
}

FString FNvimSourceCodeAccessor::NormalizeTargetPath(FString Value)
{
	Value = Value.TrimStartAndEnd();
	if (Value.IsEmpty())
	{
		return FString();
	}

	if (FPaths::IsRelative(Value))
	{
		Value = FPaths::ConvertRelativePathToFull(Value);
	}

	if (FPaths::FileExists(Value))
	{
		const FString Extension = FPaths::GetExtension(Value, true).ToLower();
		if (Extension == TEXT(".sln")
			|| Extension == TEXT(".uproject")
			|| Extension == TEXT(".code-workspace"))
		{
			return FPaths::GetPath(Value);
		}
	}

	return Value;
}

FString FNvimSourceCodeAccessor::NormalizeComparablePath(FString Value)
{
	Value = NormalizeTargetPath(Value);
	Value = Value.Replace(TEXT("\\"), TEXT("/"));
	while (Value.EndsWith(TEXT("/")))
	{
		Value.LeftChopInline(1);
	}
	return Value.ToLower();
}

FString FNvimSourceCodeAccessor::ResolveBridgeRegistryPath()
{
	FString Explicit = NormalizeExecutablePath(FPlatformMisc::GetEnvironmentVariable(TEXT("UE_UCORE_BRIDGE_REGISTRY")));
	if (!Explicit.IsEmpty())
	{
		return Explicit;
	}

	FString LocalAppData = NormalizeExecutablePath(FPlatformMisc::GetEnvironmentVariable(TEXT("LOCALAPPDATA")));
	if (LocalAppData.IsEmpty())
	{
		return FString();
	}

	return FPaths::Combine(LocalAppData, TEXT("UnrealNVIM"), TEXT("ucore-bridge.json"));
}

bool FNvimSourceCodeAccessor::ResolveSourcePath(const FString& InPath, FString& OutPath)
{
	FString Candidate = NormalizeTargetPath(InPath);
	if (Candidate.IsEmpty())
	{
		return false;
	}

	if (FPaths::FileExists(Candidate))
	{
		OutPath = Candidate;
		return true;
	}

	const FString Normalized = Candidate.Replace(TEXT("\\"), TEXT("/"));
	const FString ProjectCandidate = FPaths::ConvertRelativePathToFull(FPaths::ProjectDir() / Normalized);
	if (FPaths::FileExists(ProjectCandidate))
	{
		OutPath = ProjectCandidate;
		return true;
	}

	const FString EngineCandidate = FPaths::ConvertRelativePathToFull(FPaths::RootDir() / Normalized);
	if (FPaths::FileExists(EngineCandidate))
	{
		OutPath = EngineCandidate;
		return true;
	}

	for (const TCHAR* Marker : { TEXT("/Engine/Source/"), TEXT("/Engine/Plugins/") })
	{
		const int32 Index = Normalized.Find(Marker, ESearchCase::IgnoreCase, ESearchDir::FromStart);
		if (Index != INDEX_NONE)
		{
			const FString RelativeEnginePath = Normalized.RightChop(Index + 1);
			const FString Resolved = FPaths::ConvertRelativePathToFull(FPaths::RootDir() / RelativeEnginePath);
			if (FPaths::FileExists(Resolved))
			{
				OutPath = Resolved;
				return true;
			}
		}
	}

	return false;
}

FNvimSourceCodeAccessor::ERemoteMode FNvimSourceCodeAccessor::ResolveRemoteMode()
{
	const FString Value = NormalizeExecutablePath(FPlatformMisc::GetEnvironmentVariable(TEXT("UE_NVIM_ATTACH"))).ToLower();
	if (Value == TEXT("never") || Value == TEXT("local"))
	{
		return ERemoteMode::Never;
	}

	if (Value == TEXT("only") || Value == TEXT("remote-only"))
	{
		return ERemoteMode::Only;
	}

	if (Value == TEXT("prefer") || Value == TEXT("remote"))
	{
		return ERemoteMode::Prefer;
	}

	return ERemoteMode::Auto;
}

FString FNvimSourceCodeAccessor::ResolveServerName()
{
	for (const TCHAR* VariableName : {
		TEXT("UE_NVIM_SERVER"),
		TEXT("NVIM"),
		TEXT("NVIM_LISTEN_ADDRESS"),
		TEXT("NEOVIM_LISTEN_ADDRESS"),
		TEXT("VIM_SERVERNAME")
	})
	{
		FString Value = NormalizeExecutablePath(FPlatformMisc::GetEnvironmentVariable(VariableName));
		if (!Value.IsEmpty())
		{
			return Value;
		}
	}

	return FString();
}

bool FNvimSourceCodeAccessor::ResolveEnvironmentOverride(const TCHAR* VariableName, FExecutableLocation& OutLocation)
{
	FString Value = FPlatformMisc::GetEnvironmentVariable(VariableName);
	Value = NormalizeExecutablePath(Value);
	if (Value.IsEmpty() || !FPaths::FileExists(Value))
	{
		return false;
	}

	OutLocation.Path = Value;
	OutLocation.bIsTerminal = Value.EndsWith(TEXT("nvim.exe"), ESearchCase::IgnoreCase);
	return true;
}

bool FNvimSourceCodeAccessor::ResolvePathExecutable(const TCHAR* ExecutableName, bool bTerminal, FExecutableLocation& OutLocation)
{
	FString StdOut;
	FString StdErr;
	int32 ReturnCode = INDEX_NONE;
	FPlatformProcess::ExecProcess(
		TEXT("where.exe"),
		*FString(ExecutableName),
		&ReturnCode,
		&StdOut,
		&StdErr
	);
	if (ReturnCode != 0)
	{
		return false;
	}

	TArray<FString> Lines;
	StdOut.ParseIntoArrayLines(Lines);
	for (FString& Line : Lines)
	{
		Line = NormalizeExecutablePath(Line);
		if (!Line.IsEmpty() && FPaths::FileExists(Line))
		{
			OutLocation.Path = Line;
			OutLocation.bIsTerminal = bTerminal;
			return true;
		}
	}

	return false;
}

bool FNvimSourceCodeAccessor::ResolvePreferredShellLocation(FExecutableLocation& OutLocation)
{
	return ResolveEnvironmentOverride(TEXT("UE_POWERSHELL"), OutLocation)
		|| ResolvePathExecutable(TEXT("pwsh.exe"), false, OutLocation)
		|| ResolvePathExecutable(TEXT("powershell.exe"), false, OutLocation);
}

bool FNvimSourceCodeAccessor::ResolveExecutableLocation(FExecutableLocation& OutLocation)
{
	if (ResolveEnvironmentOverride(TEXT("UE_NVIM"), OutLocation)
		|| ResolveEnvironmentOverride(TEXT("UE_NVIM_GUI"), OutLocation)
		|| ResolveEnvironmentOverride(TEXT("NVIM_QT"), OutLocation)
		|| ResolveEnvironmentOverride(TEXT("NEOVIDE_BIN"), OutLocation)
		|| ResolveEnvironmentOverride(TEXT("NVIM_BIN"), OutLocation))
	{
		if (OutLocation.bIsTerminal && OutLocation.ShellPath.IsEmpty())
		{
			FExecutableLocation ShellLocation;
			if (ResolvePreferredShellLocation(ShellLocation))
			{
				OutLocation.ShellPath = ShellLocation.Path;
			}
		}
		return true;
	}

	if (ResolvePathExecutable(TEXT("nvim-qt.exe"), false, OutLocation)
		|| ResolvePathExecutable(TEXT("neovide.exe"), false, OutLocation)
		|| ResolvePathExecutable(TEXT("nvim.exe"), true, OutLocation))
	{
		if (OutLocation.bIsTerminal)
		{
			FExecutableLocation ShellLocation;
			if (ResolvePreferredShellLocation(ShellLocation))
			{
				OutLocation.ShellPath = ShellLocation.Path;
			}
		}
		return true;
	}

	return false;
}

bool FNvimSourceCodeAccessor::ResolveRemoteLocation(FRemoteLocation& OutLocation)
{
	const FString ServerName = ResolveServerName();
	if (ServerName.IsEmpty())
	{
		return false;
	}

	FExecutableLocation ClientLocation;
	if (!ResolvePathExecutable(TEXT("nvim.exe"), false, ClientLocation))
	{
		return false;
	}

	OutLocation.ClientPath = ClientLocation.Path;
	OutLocation.ServerName = ServerName;
	return true;
}

bool FNvimSourceCodeAccessor::ResolveBridgeRemoteLocation(const FString& ProjectRoot, FRemoteLocation& OutLocation)
{
	const FString RegistryPath = ResolveBridgeRegistryPath();
	if (RegistryPath.IsEmpty() || !FPaths::FileExists(RegistryPath))
	{
		return false;
	}

	FString RegistryText;
	if (!FFileHelper::LoadFileToString(RegistryText, *RegistryPath))
	{
		return false;
	}

	TSharedPtr<FJsonObject> RootObject;
	TSharedRef<TJsonReader<>> Reader = TJsonReaderFactory<>::Create(RegistryText);
	if (!FJsonSerializer::Deserialize(Reader, RootObject) || !RootObject.IsValid())
	{
		return false;
	}

	const TSharedPtr<FJsonObject>* ProjectsObject = nullptr;
	if (!RootObject->TryGetObjectField(TEXT("projects"), ProjectsObject) || !ProjectsObject || !ProjectsObject->IsValid())
	{
		return false;
	}

	const FString NormalizedProjectRoot = NormalizeComparablePath(ProjectRoot);
	const TSharedPtr<FJsonObject>* ProjectObject = nullptr;
	if (!NormalizedProjectRoot.IsEmpty()
		&& (!(*ProjectsObject)->TryGetObjectField(ProjectRoot, ProjectObject) || !ProjectObject || !ProjectObject->IsValid()))
	{
		for (const TPair<FString, TSharedPtr<FJsonValue>>& Pair : (*ProjectsObject)->Values)
		{
			if (NormalizeComparablePath(Pair.Key) != NormalizedProjectRoot)
			{
				continue;
			}

			if (Pair.Value.IsValid() && Pair.Value->Type == EJson::Object)
			{
				ProjectObject = &Pair.Value->AsObject();
			}
			break;
		}
	}

	FString ServerName;
	if (ProjectObject && ProjectObject->IsValid())
	{
		(*ProjectObject)->TryGetStringField(TEXT("server"), ServerName);
	}

	if (ServerName.IsEmpty())
	{
		const TSharedPtr<FJsonObject>* SessionsObject = nullptr;
		if (!RootObject->TryGetObjectField(TEXT("sessions"), SessionsObject) || !SessionsObject || !SessionsObject->IsValid())
		{
			return false;
		}

		int64 LatestSeen = -1;
		for (const TPair<FString, TSharedPtr<FJsonValue>>& Pair : (*SessionsObject)->Values)
		{
			if (!Pair.Value.IsValid() || Pair.Value->Type != EJson::Object)
			{
				continue;
			}

			const TSharedPtr<FJsonObject> SessionObject = Pair.Value->AsObject();
			if (!SessionObject.IsValid())
			{
				continue;
			}

			FString CandidateServerName;
			if (!SessionObject->TryGetStringField(TEXT("server"), CandidateServerName) || CandidateServerName.IsEmpty())
			{
				CandidateServerName = Pair.Key;
			}

			if (CandidateServerName.IsEmpty())
			{
				continue;
			}

			double CandidateLastSeenValue = 0.0;
			SessionObject->TryGetNumberField(TEXT("last_seen"), CandidateLastSeenValue);
			const int64 CandidateLastSeen = static_cast<int64>(CandidateLastSeenValue);
			if (ServerName.IsEmpty() || CandidateLastSeen >= LatestSeen)
			{
				ServerName = CandidateServerName;
				LatestSeen = CandidateLastSeen;
			}
		}
	}

	if (ServerName.IsEmpty())
	{
		return false;
	}

	FExecutableLocation ClientLocation;
	if (!ResolvePathExecutable(TEXT("nvim.exe"), false, ClientLocation))
	{
		return false;
	}

	OutLocation.ClientPath = ClientLocation.Path;
	OutLocation.ServerName = ServerName;
	return true;
}

bool FNvimSourceCodeAccessor::BuildBridgeRequestFile(const TSharedRef<FJsonObject>& RequestObject, FString& OutRequestPath)
{
	FString JsonText;
	TSharedRef<TJsonWriter<>> Writer = TJsonWriterFactory<>::Create(&JsonText);
	if (!FJsonSerializer::Serialize(RequestObject, Writer))
	{
		return false;
	}

	const FString TempDir = FPaths::ProjectIntermediateDir().IsEmpty()
		? FPaths::ProjectSavedDir()
		: FPaths::ProjectIntermediateDir();
	IFileManager::Get().MakeDirectory(*TempDir, true);

	OutRequestPath = FPaths::CreateTempFilename(*TempDir, TEXT("ucore-bridge-"), TEXT(".json"));
	return FFileHelper::SaveStringToFile(JsonText, *OutRequestPath);
}

bool FNvimSourceCodeAccessor::ShouldUseRemote() const
{
	return Remote.IsValid() && RemoteMode != ERemoteMode::Never;
}

bool FNvimSourceCodeAccessor::AllowsLocalFallback() const
{
	return RemoteMode != ERemoteMode::Only;
}

bool FNvimSourceCodeAccessor::LaunchBridgeRequest(const TSharedRef<FJsonObject>& RequestObject, const FString& ProjectRoot) const
{
	FRemoteLocation BridgeRemote;
	if (!ResolveBridgeRemoteLocation(ProjectRoot, BridgeRemote))
	{
		return false;
	}

	FString RequestPath;
	if (!BuildBridgeRequestFile(RequestObject, RequestPath))
	{
		return false;
	}

	const FString Expression = FString::Printf(
		TEXT("luaeval(\"require('ucore.bridge').handle_request_file(_A)\", %s)"),
		*QuoteVimLiteral(RequestPath.Replace(TEXT("\\"), TEXT("/")))
	);

	TArray<FString> RemoteArgs;
	RemoteArgs.Add(TEXT("--server"));
	RemoteArgs.Add(BridgeRemote.ServerName);
	RemoteArgs.Add(TEXT("--remote-expr"));
	RemoteArgs.Add(Expression);

	FString ArgString;
	for (const FString& Arg : RemoteArgs)
	{
		if (!ArgString.IsEmpty())
		{
			ArgString.AppendChar(TEXT(' '));
		}
		ArgString.Append(QuoteArg(Arg));
	}

	FString StdOut;
	FString StdErr;
	int32 ReturnCode = INDEX_NONE;
	FPlatformProcess::ExecProcess(
		*BridgeRemote.ClientPath,
		*ArgString,
		&ReturnCode,
		&StdOut,
		&StdErr
	);

	const bool bStdoutOk = StdOut.TrimStartAndEnd().Equals(TEXT("ok"), ESearchCase::IgnoreCase);
	const bool bRequestConsumed = IFileManager::Get().FileSize(*RequestPath) == INDEX_NONE;
	if (ReturnCode == 0 && (bStdoutOk || bRequestConsumed))
	{
		return true;
	}

	IFileManager::Get().Delete(*RequestPath, false, true);
	return false;
}

bool FNvimSourceCodeAccessor::LaunchRemote(const TArray<FString>& Args, bool bDirectoryTarget) const
{
	if (!Remote.IsValid())
	{
		return false;
	}

	TArray<FString> RemoteArgs;
	RemoteArgs.Add(TEXT("--server"));
	RemoteArgs.Add(Remote.ServerName);

	if (bDirectoryTarget)
	{
		RemoteArgs.Add(TEXT("--remote-silent"));
		for (const FString& Arg : Args)
		{
			RemoteArgs.Add(Arg);
		}
		RemoteArgs.Add(TEXT("-c"));
		RemoteArgs.Add(TEXT("doautocmd BufEnter"));
	}
	else
	{
		for (const FString& Arg : Args)
		{
			RemoteArgs.Add(Arg);
		}
	}

	FString ArgString;
	for (const FString& Arg : RemoteArgs)
	{
		if (!ArgString.IsEmpty())
		{
			ArgString.AppendChar(TEXT(' '));
		}
		ArgString.Append(QuoteArg(Arg));
	}

	uint32 ProcessId = 0;
	FProcHandle Proc = FPlatformProcess::CreateProc(
		*Remote.ClientPath,
		*ArgString,
		true,
		false,
		false,
		&ProcessId,
		0,
		nullptr,
		nullptr
	);
	return Proc.IsValid();
}

bool FNvimSourceCodeAccessor::Launch(const TArray<FString>& Args) const
{
	if (!Location.IsValid())
	{
		return false;
	}

	FString ShellPath = Location.ShellPath;
	if (Location.bIsTerminal && ShellPath.IsEmpty())
	{
		FExecutableLocation ShellLocation;
		if (ResolvePreferredShellLocation(ShellLocation))
		{
			ShellPath = ShellLocation.Path;
		}
	}

	if (Location.bIsTerminal && !ShellPath.IsEmpty())
	{
		FString Command = FString::Printf(TEXT("& %s"), *QuotePowerShellLiteral(Location.Path));
		for (const FString& Arg : Args)
		{
			Command.AppendChar(TEXT(' '));
			Command.Append(QuotePowerShellLiteral(Arg));
		}

		const FString ShellArgs = FString::Printf(
			TEXT("-NoLogo -NoExit -Command %s"),
			*QuoteArg(Command)
		);

		uint32 ShellProcessId = 0;
		FProcHandle ShellProc = FPlatformProcess::CreateProc(
			*ShellPath,
			*ShellArgs,
			false,
			false,
			false,
			&ShellProcessId,
			0,
			nullptr,
			nullptr
		);
		return ShellProc.IsValid();
	}

	FString ArgString;
	for (const FString& Arg : Args)
	{
		if (!ArgString.IsEmpty())
		{
			ArgString.AppendChar(TEXT(' '));
		}
		ArgString.Append(QuoteArg(Arg));
	}

	uint32 ProcessId = 0;
	FProcHandle Proc = FPlatformProcess::CreateProc(
		*Location.Path,
		*ArgString,
		false,
		false,
		false,
		&ProcessId,
		0,
		nullptr,
		nullptr
	);
	return Proc.IsValid();
}

#undef LOCTEXT_NAMESPACE
