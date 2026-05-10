using UnrealBuildTool;

public class NeovimLink : ModuleRules
{
	public NeovimLink(ReadOnlyTargetRules Target) : base(Target)
	{
		PCHUsage = ModuleRules.PCHUsageMode.UseExplicitOrSharedPCHs;

		PublicDependencyModuleNames.AddRange(
			new string[]
			{
				"Core",
			}
			);

		PrivateDependencyModuleNames.AddRange(
			new string[]
			{
				"CoreUObject",
				"AssetRegistry",
				"Engine",
				"Json",
				"JsonUtilities",
				"Projects",
				"Slate",
				"SlateCore",
				"UnrealEd"
			}
			);
	}
}
