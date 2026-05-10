#pragma once

#include "Modules/ModuleInterface.h"
#include "NvimSourceCodeAccessor.h"
#include "Templates/UniquePtr.h"

class FNvimSourceCodeNavigationHandler;

class FNvimSourceCodeAccessModule : public IModuleInterface
{
public:
	virtual void StartupModule() override;
	virtual void ShutdownModule() override;

	FNvimSourceCodeAccessor& GetAccessor();

private:
	FNvimSourceCodeAccessor Accessor;
	TUniquePtr<FNvimSourceCodeNavigationHandler> NavigationHandler;
};
