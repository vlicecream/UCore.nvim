#include "NvimSourceCodeNavigationHandler.h"

#include "NvimSourceCodeAccessor.h"

#include "HAL/FileManager.h"
#include "ISourceCodeAccessModule.h"
#include "Misc/FileHelper.h"
#include "Misc/Paths.h"
#include "Modules/ModuleManager.h"
#include "UObject/Class.h"
#include "UObject/Field.h"
#include "UObject/UnrealType.h"

FNvimSourceCodeNavigationHandler::FNvimSourceCodeNavigationHandler(FNvimSourceCodeAccessor& InAccessor)
	: Accessor(InAccessor)
{
}

bool FNvimSourceCodeNavigationHandler::CanNavigateToClass(const UClass* InClass)
{
	if (!IsActiveAccessor())
	{
		return false;
	}

	FString HeaderPath;
	return InClass != nullptr && InClass->HasAllClassFlags(CLASS_Native) && ResolveHeaderPath(InClass, HeaderPath);
}

bool FNvimSourceCodeNavigationHandler::NavigateToClass(const UClass* InClass)
{
	if (!IsActiveAccessor())
	{
		return false;
	}

	if (!CanNavigateToClass(InClass))
	{
		return false;
	}

	const FString HeaderPath = NormalizeReadablePath([&]() -> FString
	{
		FString Path;
		ResolveHeaderPath(InClass, Path);
		return Path;
	}());
	const int32 LineNumber = FindClassLine(HeaderPath, GetCppTypeName(InClass), false);
	return OpenAtLine(HeaderPath, LineNumber);
}

bool FNvimSourceCodeNavigationHandler::CanNavigateToStruct(const UScriptStruct* InStruct)
{
	if (!IsActiveAccessor())
	{
		return false;
	}

	return CanNavigateToStruct(static_cast<const UStruct*>(InStruct));
}

bool FNvimSourceCodeNavigationHandler::NavigateToStruct(const UScriptStruct* InStruct)
{
	if (!IsActiveAccessor())
	{
		return false;
	}

	return NavigateToStruct(static_cast<const UStruct*>(InStruct));
}

bool FNvimSourceCodeNavigationHandler::CanNavigateToFunction(const UFunction* InFunction)
{
	if (!IsActiveAccessor())
	{
		return false;
	}

	if (!InFunction)
	{
		return false;
	}

	const UClass* OwningClass = InFunction->GetOwnerClass();
	if (!OwningClass || !OwningClass->HasAllClassFlags(CLASS_Native))
	{
		return false;
	}

	FString SourcePath;
	if (ResolveSourcePath(OwningClass, SourcePath))
	{
		return true;
	}

	FString HeaderPath;
	return ResolveHeaderPath(OwningClass, HeaderPath);
}

bool FNvimSourceCodeNavigationHandler::NavigateToFunction(const UFunction* InFunction)
{
	if (!IsActiveAccessor())
	{
		return false;
	}

	if (!CanNavigateToFunction(InFunction))
	{
		return false;
	}

	const UClass* OwningClass = InFunction->GetOwnerClass();
	const FString OwnerTypeName = GetCppTypeName(OwningClass);

	FString SourcePath;
	if (ResolveSourcePath(OwningClass, SourcePath))
	{
		SourcePath = NormalizeReadablePath(SourcePath);
		const int32 LineNumber = FindFunctionLine(SourcePath, OwnerTypeName, InFunction->GetName());
		if (LineNumber > 0)
		{
			return OpenAtLine(SourcePath, LineNumber);
		}
	}

	FString HeaderPath;
	if (ResolveHeaderPath(OwningClass, HeaderPath))
	{
		HeaderPath = NormalizeReadablePath(HeaderPath);
		const int32 LineNumber = FindFunctionLine(HeaderPath, FString(), InFunction->GetName());
		return OpenAtLine(HeaderPath, LineNumber);
	}

	return false;
}

bool FNvimSourceCodeNavigationHandler::CanNavigateToProperty(const FProperty* InProperty)
{
	if (!IsActiveAccessor())
	{
		return false;
	}

	if (!InProperty || !InProperty->IsNative())
	{
		return false;
	}

	FString HeaderPath;
	return ResolveHeaderPath(InProperty->GetOwnerUField(), HeaderPath);
}

bool FNvimSourceCodeNavigationHandler::NavigateToProperty(const FProperty* InProperty)
{
	if (!IsActiveAccessor())
	{
		return false;
	}

	if (!CanNavigateToProperty(InProperty))
	{
		return false;
	}

	FString HeaderPath;
	if (!ResolveHeaderPath(InProperty->GetOwnerUField(), HeaderPath))
	{
		return false;
	}

	HeaderPath = NormalizeReadablePath(HeaderPath);
	const int32 LineNumber = FindPropertyLine(HeaderPath, InProperty->GetName());
	return OpenAtLine(HeaderPath, LineNumber);
}

bool FNvimSourceCodeNavigationHandler::CanNavigateToStruct(const UStruct* InStruct)
{
	if (!IsActiveAccessor())
	{
		return false;
	}

	if (!InStruct || !InStruct->IsNative())
	{
		return false;
	}

	FString HeaderPath;
	return ResolveHeaderPath(InStruct, HeaderPath);
}

bool FNvimSourceCodeNavigationHandler::NavigateToStruct(const UStruct* InStruct)
{
	if (!IsActiveAccessor())
	{
		return false;
	}

	return NavigateToStructInternal(InStruct);
}

bool FNvimSourceCodeNavigationHandler::ResolveHeaderPath(const UField* InField, FString& OutPath)
{
	return InField != nullptr
		&& FSourceCodeNavigation::FindClassHeaderPath(ResolveOwningField(InField), OutPath)
		&& IFileManager::Get().FileSize(*OutPath) != INDEX_NONE;
}

bool FNvimSourceCodeNavigationHandler::ResolveSourcePath(const UField* InField, FString& OutPath)
{
	return InField != nullptr
		&& FSourceCodeNavigation::FindClassSourcePath(ResolveOwningField(InField), OutPath)
		&& IFileManager::Get().FileSize(*OutPath) != INDEX_NONE;
}

FString FNvimSourceCodeNavigationHandler::NormalizeReadablePath(const FString& InPath)
{
	if (InPath.IsEmpty())
	{
		return FString();
	}

	return IFileManager::Get().ConvertToAbsolutePathForExternalAppForRead(*InPath);
}

int32 FNvimSourceCodeNavigationHandler::FindClassLine(const FString& FilePath, const FString& TypeName, bool bIsStruct)
{
	TArray<FString> Tokens;
	Tokens.Add(FString::Printf(TEXT("%s %s"), bIsStruct ? TEXT("struct") : TEXT("class"), *TypeName));
	Tokens.Add(FString::Printf(TEXT("%s\t%s"), bIsStruct ? TEXT("struct") : TEXT("class"), *TypeName));
	return FindTokenLine(FilePath, Tokens, TypeName);
}

int32 FNvimSourceCodeNavigationHandler::FindFunctionLine(const FString& FilePath, const FString& OwnerTypeName, const FString& FunctionName)
{
	TArray<FString> Tokens;
	if (!OwnerTypeName.IsEmpty())
	{
		Tokens.Add(FString::Printf(TEXT("%s::%s("), *OwnerTypeName, *FunctionName));
		Tokens.Add(FString::Printf(TEXT("%s::%s ("), *OwnerTypeName, *FunctionName));
		Tokens.Add(FString::Printf(TEXT("%s::%s"), *OwnerTypeName, *FunctionName));
	}

	Tokens.Add(FString::Printf(TEXT("%s("), *FunctionName));
	Tokens.Add(FString::Printf(TEXT("%s ("), *FunctionName));
	return FindTokenLine(FilePath, Tokens, FunctionName);
}

int32 FNvimSourceCodeNavigationHandler::FindPropertyLine(const FString& FilePath, const FString& PropertyName)
{
	TArray<FString> Tokens;
	Tokens.Add(FString::Printf(TEXT("%s;"), *PropertyName));
	Tokens.Add(FString::Printf(TEXT("%s ="), *PropertyName));
	Tokens.Add(FString::Printf(TEXT("%s\t"), *PropertyName));
	return FindTokenLine(FilePath, Tokens, PropertyName);
}

int32 FNvimSourceCodeNavigationHandler::FindTokenLine(const FString& FilePath, const TArray<FString>& Tokens, const FString& Identifier)
{
	TArray<FString> Lines;
	if (!FFileHelper::LoadFileToStringArray(Lines, *FilePath))
	{
		return 1;
	}

	for (int32 LineIndex = 0; LineIndex < Lines.Num(); ++LineIndex)
	{
		const FString& Line = Lines[LineIndex];
		bool bTokenMatched = Tokens.Num() == 0;
		for (const FString& Token : Tokens)
		{
			if (!Token.IsEmpty() && Line.Contains(Token))
			{
				bTokenMatched = true;
				break;
			}
		}

		if (!bTokenMatched)
		{
			continue;
		}

		if (Identifier.IsEmpty() || ContainsIdentifier(Line, Identifier))
		{
			return LineIndex + 1;
		}
	}

	return 1;
}

bool FNvimSourceCodeNavigationHandler::ContainsIdentifier(const FString& Line, const FString& Identifier)
{
	if (Identifier.IsEmpty())
	{
		return false;
	}

	int32 SearchFrom = 0;
	while (true)
	{
		const int32 Index = Line.Find(Identifier, ESearchCase::CaseSensitive, ESearchDir::FromStart, SearchFrom);
		if (Index == INDEX_NONE)
		{
			return false;
		}

		const bool bStartBoundary = Index == 0
			|| !(FChar::IsAlnum(Line[Index - 1]) || Line[Index - 1] == TEXT('_'));
		const int32 EndIndex = Index + Identifier.Len();
		const bool bEndBoundary = EndIndex >= Line.Len()
			|| !(FChar::IsAlnum(Line[EndIndex]) || Line[EndIndex] == TEXT('_'));

		if (bStartBoundary && bEndBoundary)
		{
			return true;
		}

		SearchFrom = Index + Identifier.Len();
	}
}

FString FNvimSourceCodeNavigationHandler::GetCppTypeName(const UStruct* InStruct)
{
	if (!InStruct)
	{
		return FString();
	}

	return InStruct->GetPrefixCPP() + InStruct->GetName();
}

const UField* FNvimSourceCodeNavigationHandler::ResolveOwningField(const UField* InField)
{
	if (!InField)
	{
		return nullptr;
	}

	if (const UFunction* Function = Cast<UFunction>(InField))
	{
		if (const UClass* OwnerClass = Function->GetOwnerClass())
		{
			return OwnerClass;
		}
	}

	return InField;
}

bool FNvimSourceCodeNavigationHandler::IsActiveAccessor() const
{
	ISourceCodeAccessModule* SourceCodeAccessModule =
		FModuleManager::GetModulePtr<ISourceCodeAccessModule>(TEXT("SourceCodeAccess"));
	if (!SourceCodeAccessModule)
	{
		return false;
	}

	return SourceCodeAccessModule->GetAccessor().GetFName() == Accessor.GetFName();
}

bool FNvimSourceCodeNavigationHandler::OpenAtLine(const FString& FilePath, int32 LineNumber) const
{
	return Accessor.OpenFileAtLine(FilePath, LineNumber > 0 ? LineNumber : 1, 1);
}

bool FNvimSourceCodeNavigationHandler::NavigateToStructInternal(const UStruct* InStruct)
{
	if (!CanNavigateToStruct(InStruct))
	{
		return false;
	}

	FString HeaderPath;
	if (!ResolveHeaderPath(InStruct, HeaderPath))
	{
		return false;
	}

	HeaderPath = NormalizeReadablePath(HeaderPath);
	const bool bIsStruct = !InStruct->IsA<UClass>();
	const int32 LineNumber = FindClassLine(HeaderPath, GetCppTypeName(InStruct), bIsStruct);
	return OpenAtLine(HeaderPath, LineNumber);
}
