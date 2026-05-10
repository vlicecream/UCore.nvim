#pragma once

#include "ISourceCodeAccessor.h"

class FJsonObject;

class FNvimSourceCodeAccessor : public ISourceCodeAccessor
{
public:
	void Startup();
	void Shutdown();

	virtual void RefreshAvailability() override;
	virtual bool CanAccessSourceCode() const override;
	virtual FName GetFName() const override;
	virtual FText GetNameText() const override;
	virtual FText GetDescriptionText() const override;
	virtual bool OpenSolution() override;
	virtual bool OpenSolutionAtPath(const FString& InSolutionPath) override;
	virtual bool DoesSolutionExist() const override;
	virtual bool OpenFileAtLine(const FString& FullPath, int32 LineNumber, int32 ColumnNumber = 0) override;
	virtual bool OpenSourceFiles(const TArray<FString>& AbsoluteSourcePaths) override;
	virtual bool AddSourceFiles(const TArray<FString>& AbsoluteSourcePaths, const TArray<FString>& AvailableModules) override;
	virtual bool SaveAllOpenDocuments() const override;
	virtual void Tick(const float DeltaTime) override;

private:
	enum class ERemoteMode : uint8
	{
		Auto,
		Prefer,
		Only,
		Never,
	};

	struct FExecutableLocation
	{
		FString Path;
		FString ShellPath;
		bool bIsTerminal = false;

		bool IsValid() const
		{
			return !Path.IsEmpty();
		}
	};

	struct FRemoteLocation
	{
		FString ClientPath;
		FString ServerName;

		bool IsValid() const
		{
			return !ClientPath.IsEmpty() && !ServerName.IsEmpty();
		}
	};

	FExecutableLocation Location;
	FRemoteLocation Remote;
	ERemoteMode RemoteMode = ERemoteMode::Auto;

	static FString QuoteArg(const FString& Value);
	static FString QuotePowerShellLiteral(const FString& Value);
	static FString QuoteVimLiteral(const FString& Value);
	static FString NormalizeExecutablePath(FString Value);
	static FString NormalizeTargetPath(FString Value);
	static FString NormalizeComparablePath(FString Value);
	static FString ResolveBridgeRegistryPath();
	static bool ResolveSourcePath(const FString& InPath, FString& OutPath);
	static ERemoteMode ResolveRemoteMode();
	static FString ResolveServerName();
	static bool ResolveEnvironmentOverride(const TCHAR* VariableName, FExecutableLocation& OutLocation);
	static bool ResolvePathExecutable(const TCHAR* ExecutableName, bool bTerminal, FExecutableLocation& OutLocation);
	static bool ResolveExecutableLocation(FExecutableLocation& OutLocation);
	static bool ResolveRemoteLocation(FRemoteLocation& OutLocation);
	static bool ResolveBridgeRemoteLocation(const FString& ProjectRoot, FRemoteLocation& OutLocation);
	static bool BuildBridgeRequestFile(const TSharedRef<FJsonObject>& RequestObject, FString& OutRequestPath);
	bool ShouldUseRemote() const;
	bool AllowsLocalFallback() const;
	bool LaunchBridgeRequest(const TSharedRef<FJsonObject>& RequestObject, const FString& ProjectRoot) const;
	bool LaunchRemote(const TArray<FString>& Args, bool bDirectoryTarget = false) const;
	bool Launch(const TArray<FString>& Args) const;
};
