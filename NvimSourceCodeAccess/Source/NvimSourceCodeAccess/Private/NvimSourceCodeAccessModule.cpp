#include "NvimSourceCodeAccessModule.h"

#include "Features/IModularFeatures.h"
#include "Modules/ModuleManager.h"
#include "NvimSourceCodeNavigationHandler.h"
#include "SourceCodeNavigation.h"

IMPLEMENT_MODULE(FNvimSourceCodeAccessModule, NvimSourceCodeAccess)

void FNvimSourceCodeAccessModule::StartupModule()
{
	Accessor.Startup();
	NavigationHandler = MakeUnique<FNvimSourceCodeNavigationHandler>(Accessor);
	IModularFeatures::Get().RegisterModularFeature(TEXT("SourceCodeAccessor"), &Accessor);
	FSourceCodeNavigation::AddNavigationHandler(NavigationHandler.Get());
}

void FNvimSourceCodeAccessModule::ShutdownModule()
{
	if (NavigationHandler)
	{
		FSourceCodeNavigation::RemoveNavigationHandler(NavigationHandler.Get());
		NavigationHandler.Reset();
	}
	IModularFeatures::Get().UnregisterModularFeature(TEXT("SourceCodeAccessor"), &Accessor);
	Accessor.Shutdown();
}

FNvimSourceCodeAccessor& FNvimSourceCodeAccessModule::GetAccessor()
{
	return Accessor;
}
