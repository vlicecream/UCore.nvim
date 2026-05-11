#pragma once

#include "CoreMinimal.h"
#include "SourceCodeNavigation.h"

class FNvimSourceCodeAccessor;
class FProperty;
class UClass;
class UField;
class UFunction;
class UScriptStruct;
class UStruct;

class FNvimSourceCodeNavigationHandler : public ISourceCodeNavigationHandler
{
public:
	explicit FNvimSourceCodeNavigationHandler(FNvimSourceCodeAccessor& InAccessor);

	virtual bool CanNavigateToClass(const UClass* InClass) override;
	virtual bool NavigateToClass(const UClass* InClass) override;

	virtual bool CanNavigateToStruct(const UScriptStruct* InStruct) override;
	virtual bool NavigateToStruct(const UScriptStruct* InStruct) override;

	virtual bool CanNavigateToFunction(const UFunction* InFunction) override;
	virtual bool NavigateToFunction(const UFunction* InFunction) override;

	virtual bool CanNavigateToProperty(const FProperty* InProperty) override;
	virtual bool NavigateToProperty(const FProperty* InProperty) override;

	virtual bool CanNavigateToStruct(const UStruct* InStruct) override;
	virtual bool NavigateToStruct(const UStruct* InStruct) override;

private:
	FNvimSourceCodeAccessor& Accessor;

	static bool ResolveHeaderPath(const UField* InField, FString& OutPath);
	static bool ResolveSourcePath(const UField* InField, FString& OutPath);
	static FString NormalizeReadablePath(const FString& InPath);
	static int32 FindClassLine(const FString& FilePath, const FString& TypeName, bool bIsStruct);
	static int32 FindFunctionLine(const FString& FilePath, const FString& OwnerTypeName, const FString& FunctionName);
	static int32 FindPropertyLine(const FString& FilePath, const FString& PropertyName);
	static int32 FindTokenLine(const FString& FilePath, const TArray<FString>& Tokens, const FString& Identifier = FString());
	static bool ContainsIdentifier(const FString& Line, const FString& Identifier);
	static FString GetCppTypeName(const UStruct* InStruct);
	static const UField* ResolveOwningField(const UField* InField);

	bool IsActiveAccessor() const;
	bool OpenAtLine(const FString& FilePath, int32 LineNumber) const;
	bool NavigateToStructInternal(const UStruct* InStruct);
};
