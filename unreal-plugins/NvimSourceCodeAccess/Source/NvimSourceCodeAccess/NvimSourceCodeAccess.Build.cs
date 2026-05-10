using UnrealBuildTool;

public class NvimSourceCodeAccess : ModuleRules
{
	public NvimSourceCodeAccess(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = ModuleRules.PCHUsageMode.UseExplicitOrSharedPCHs;

		PublicDependencyModuleNames.AddRange(
			new string[]
			{
				"Core",
				"CoreUObject",
			}
		);

		PrivateDependencyModuleNames.AddRange(
			new string[]
			{
				"Json",
				"SourceCodeAccess",
				"UnrealEd",
			}
		);
	}
}
