#include "NeovimLink.h"

#include "AssetRegistry/AssetRegistryModule.h"
#include "Dom/JsonObject.h"
#include "Editor.h"
#include "HAL/FileManager.h"
#include "HAL/PlatformMisc.h"
#include "HAL/PlatformProcess.h"
#include "Misc/FileHelper.h"
#include "Misc/Paths.h"
#include "Serialization/JsonSerializer.h"
#include "Serialization/JsonWriter.h"
#include "Subsystems/AssetEditorSubsystem.h"

namespace NeovimLink
{
	static FString NormalizePath(FString Path)
	{
		if (Path.IsEmpty())
		{
			return FString();
		}

		FPaths::NormalizeFilename(Path);
		while (Path.EndsWith(TEXT("/")))
		{
			Path.LeftChopInline(1, EAllowShrinking::No);
		}
		return Path;
	}

	static FString ResolveBaseDir()
	{
		const FString Explicit = FPlatformMisc::GetEnvironmentVariable(TEXT("UE_UCORE_BRIDGE_DIR"));
		if (!Explicit.IsEmpty())
		{
			return NormalizePath(Explicit);
		}

		const FString LocalAppData = FPlatformMisc::GetEnvironmentVariable(TEXT("LOCALAPPDATA"));
		if (!LocalAppData.IsEmpty())
		{
			return NormalizePath(FPaths::Combine(LocalAppData, TEXT("UnrealNVIM")));
		}

		return NormalizePath(FPaths::Combine(FPlatformProcess::UserSettingsDir(), TEXT("UnrealNVIM")));
	}

	static FString ResolveRequestDir()
	{
		return NormalizePath(FPaths::Combine(ResolveBaseDir(), TEXT("unreal-requests")));
	}

	static FString ResolveSessionDir()
	{
		return NormalizePath(FPaths::Combine(ResolveBaseDir(), TEXT("unreal-sessions")));
	}

	static FString ResolveProjectRoot()
	{
		return NormalizePath(FPaths::ConvertRelativePathToFull(FPaths::ProjectDir()));
	}

	static FString SessionFileName()
	{
		return FString::Printf(TEXT("editor-%d.json"), FPlatformProcess::GetCurrentProcessId());
	}

	static bool ReadJsonFile(const FString& FilePath, TSharedPtr<FJsonObject>& OutObject)
	{
		FString Text;
		if (!FFileHelper::LoadFileToString(Text, *FilePath))
		{
			return false;
		}

		TSharedRef<TJsonReader<>> Reader = TJsonReaderFactory<>::Create(Text);
		return FJsonSerializer::Deserialize(Reader, OutObject) && OutObject.IsValid();
	}

	static bool WriteJsonFile(const FString& FilePath, const TSharedRef<FJsonObject>& Object)
	{
		FString Text;
		TSharedRef<TJsonWriter<>> Writer = TJsonWriterFactory<>::Create(&Text);
		if (!FJsonSerializer::Serialize(Object, Writer))
		{
			return false;
		}

		return FFileHelper::SaveStringToFile(Text, *FilePath);
	}
}

void FNeovimLinkModule::StartupModule()
{
	RequestDirectory = NeovimLink::ResolveRequestDir();
	SessionDirectory = NeovimLink::ResolveSessionDir();
	CurrentProjectRoot = NeovimLink::ResolveProjectRoot();
	SessionFilePath = FPaths::Combine(SessionDirectory, NeovimLink::SessionFileName());

	IFileManager::Get().MakeDirectory(*RequestDirectory, true);
	IFileManager::Get().MakeDirectory(*SessionDirectory, true);
	WriteSessionHeartbeat(true);

	TickerHandle = FTSTicker::GetCoreTicker().AddTicker(
		FTickerDelegate::CreateRaw(this, &FNeovimLinkModule::HandleTicker),
		0.5f
	);
}

void FNeovimLinkModule::ShutdownModule()
{
	if (TickerHandle.IsValid())
	{
		FTSTicker::GetCoreTicker().RemoveTicker(TickerHandle);
		TickerHandle.Reset();
	}

	IFileManager::Get().Delete(*SessionFilePath, false, true, true);
}

bool FNeovimLinkModule::HandleTicker(float DeltaTime)
{
	(void)DeltaTime;
	WriteSessionHeartbeat(false);
	ProcessRequests();
	return true;
}

void FNeovimLinkModule::WriteSessionHeartbeat(bool bForce)
{
	const double Now = FPlatformTime::Seconds();
	if (!bForce && (Now - LastHeartbeatWriteSeconds) < 1.0)
	{
		return;
	}

	LastHeartbeatWriteSeconds = Now;
	IFileManager::Get().MakeDirectory(*SessionDirectory, true);

	TSharedRef<FJsonObject> Object = MakeShared<FJsonObject>();
	Object->SetStringField(TEXT("project_root"), CurrentProjectRoot);
	Object->SetNumberField(TEXT("pid"), FPlatformProcess::GetCurrentProcessId());
	Object->SetNumberField(TEXT("last_seen"), FDateTime::UtcNow().ToUnixTimestamp());

	NeovimLink::WriteJsonFile(SessionFilePath, Object);
}

void FNeovimLinkModule::ProcessRequests()
{
	TArray<FString> Names;
	IFileManager::Get().FindFiles(Names, *FPaths::Combine(RequestDirectory, TEXT("*.json")), true, false);
	Names.Sort();

	for (const FString& Name : Names)
	{
		ProcessRequestFile(FPaths::Combine(RequestDirectory, Name));
	}
}

void FNeovimLinkModule::ProcessRequestFile(const FString& FilePath)
{
	TSharedPtr<FJsonObject> Object;
	if (!NeovimLink::ReadJsonFile(FilePath, Object) || !Object.IsValid())
	{
		IFileManager::Get().Delete(*FilePath, false, true, true);
		return;
	}

	FString RequestProjectRoot;
	Object->TryGetStringField(TEXT("project_root"), RequestProjectRoot);
	RequestProjectRoot = NeovimLink::NormalizePath(RequestProjectRoot);
	if (!RequestProjectRoot.IsEmpty() && RequestProjectRoot != CurrentProjectRoot)
	{
		return;
	}

	FString Kind;
	Object->TryGetStringField(TEXT("kind"), Kind);
	if (Kind == TEXT("open_asset"))
	{
		FString AssetPath;
		if (Object->TryGetStringField(TEXT("asset_path"), AssetPath))
		{
			OpenAssetPath(AssetPath);
		}
	}

	IFileManager::Get().Delete(*FilePath, false, true, true);
}

void FNeovimLinkModule::OpenAssetPath(const FString& AssetPath)
{
	if (AssetPath.IsEmpty() || GEditor == nullptr)
	{
		return;
	}

	FAssetRegistryModule& AssetRegistryModule =
		FModuleManager::LoadModuleChecked<FAssetRegistryModule>(TEXT("AssetRegistry"));

	TArray<FAssetData> Assets;
	AssetRegistryModule.Get().GetAssetsByPackageName(FName(*AssetPath), Assets);

	UObject* AssetObject = nullptr;
	for (const FAssetData& AssetData : Assets)
	{
		AssetObject = AssetData.GetAsset();
		if (AssetObject != nullptr)
		{
			break;
		}
	}

	if (AssetObject == nullptr && AssetPath.Contains(TEXT(".")))
	{
		AssetObject = StaticLoadObject(UObject::StaticClass(), nullptr, *AssetPath);
	}

	if (AssetObject == nullptr)
	{
		return;
	}

	if (UAssetEditorSubsystem* AssetEditorSubsystem = GEditor->GetEditorSubsystem<UAssetEditorSubsystem>())
	{
		AssetEditorSubsystem->OpenEditorForAsset(AssetObject);
	}
}

IMPLEMENT_MODULE(FNeovimLinkModule, NeovimLink)
