-- False-positive guard: the modern ALTER DATABASE ... SET syntax replaces the
-- removed sp_dboption procedure. No sp_dboption call appears, so
-- deprecated.sp_dboption must stay silent.
ALTER DATABASE pubs SET READ_ONLY;
