#pragma once

#include "CoreMinimal.h"
#include "Containers/Ticker.h"
#include "Modules/ModuleManager.h"

class FNeovimLinkModule : public IModuleInterface
{
public:
	virtual void StartupModule() override;
	virtual void ShutdownModule() override;

private:
	bool HandleTicker(float DeltaTime);
	void WriteSessionHeartbeat(bool bForce);
	void ProcessRequests();
	void ProcessRequestFile(const FString& FilePath);
	void OpenAssetPath(const FString& AssetPath);

	FTSTicker::FDelegateHandle TickerHandle;
	FString RequestDirectory;
	FString SessionDirectory;
	FString SessionFilePath;
	FString CurrentProjectRoot;
	double LastHeartbeatWriteSeconds = 0.0;
};
