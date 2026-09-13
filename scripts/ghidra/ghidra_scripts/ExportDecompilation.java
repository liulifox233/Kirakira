// Exports decompiled C and a string table for the program a headless run
// opened.
//
// Driven by scripts/ghidra/run.sh as a post-script:
//
//   -postScript ExportDecompilation.java <outdir> <perFunctionTimeoutSeconds>
//
// Output, under <outdir>/decompiled/:
//   <va>_<name>.c   the C for one function, preceded by a VA/name/prototype
//                   comment block
//   index.tsv       va <tab> name <tab> file <tab> outcome <tab> detail
//                   (detail is the prototype, or the failure message)
//   strings.tsv     va <tab> data type <tab> text for every string of three
//                   characters or more, ASCII or UTF-16
// so a later analysis can locate a function by VA (e.g. 0x1001d000) or by
// name from the export without re-running Ghidra.
//
// A string table is not optional for the E-mote plugins: the property-name
// pool holds entries such as "opa" (UTF-16, VA 0x100e1490 in
// motionplayer_nod3d.dll) that are only ever referenced as a pointer, and
// Ghidra's auto string analyzer cannot define them — its minimum length
// starts at four characters (StringsAnalyzer$MinStringLen LEN_4), so a
// three-character name stays DAT_xxxxxxxx in the C.  The table resolves
// those addresses back to text.
//
// Three characters is the floor rather than a guess: it is what the property
// pool needs, and going down to two adds printable byte runs from code
// (measured on menu.dll: 378 extra 8-bit hits against 69 extra UTF-16 ones,
// taking the table from 920 to 1367 entries).
//
//@category Kirakira

import java.io.File;
import java.io.PrintWriter;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.listing.Function;
import ghidra.program.util.string.FoundString;
import ghidra.program.util.string.StringSearcher;

public class ExportDecompilation extends GhidraScript {

	private static final int DEFAULT_TIMEOUT_SECONDS = 60;
	private static final int MAX_NAME_LENGTH = 96;
	private static final int MIN_STRING_LENGTH = 3;

	@Override
	public void run() throws Exception {
		String[] args = getScriptArgs();
		if (args.length < 1) {
			printerr("usage: ExportDecompilation <outdir> [timeoutSeconds]");
			return;
		}

		File outDir = new File(args[0], "decompiled");
		if (!outDir.isDirectory() && !outDir.mkdirs()) {
			printerr("cannot create output directory " + outDir);
			return;
		}
		int timeoutSeconds = args.length > 1 ? Integer.parseInt(args[1]) : DEFAULT_TIMEOUT_SECONDS;

		exportFunctions(outDir, timeoutSeconds);
		exportStrings(outDir);
	}

	private void exportFunctions(File outDir, int timeoutSeconds) throws Exception {
		DecompInterface decompiler = new DecompInterface();
		if (!decompiler.openProgram(currentProgram)) {
			printerr("decompiler cannot open " + currentProgram.getName());
			return;
		}

		int exported = 0;
		int failed = 0;
		int skipped = 0;
		PrintWriter index = new PrintWriter(new File(outDir, "index.tsv"), "UTF-8");
		try {
			index.println("# va\tname\tfile\toutcome\tdetail");
			for (Function function : currentProgram.getFunctionManager().getFunctions(true)) {
				if (monitor.isCancelled()) {
					println("cancelled after " + (exported + failed) + " functions");
					break;
				}
				if (function.isExternal()) {
					skipped++;
					continue;
				}

				String va = function.getEntryPoint().toString();
				String name = function.getName();
				String detail = "";
				String c;
				String outcome;
				try {
					DecompileResults results = decompiler.decompileFunction(function, timeoutSeconds, monitor);
					if (results.decompileCompleted() && results.getDecompiledFunction() != null) {
						c = results.getDecompiledFunction().getC();
						outcome = "ok";
						detail = function.getSignature().toString();
						exported++;
					} else {
						c = "/* decompilation failed: " + results.getErrorMessage() + " */";
						outcome = "failed";
						detail = String.valueOf(results.getErrorMessage());
						failed++;
					}
				} catch (Exception e) {
					c = "/* decompilation error: " + e + " */";
					outcome = "error";
					detail = e.toString();
					failed++;
				}

				File file = new File(outDir, va + "_" + sanitize(name) + ".c");
				PrintWriter writer = new PrintWriter(file, "UTF-8");
				try {
					writer.println("/* " + va + "  " + name + " */");
					writer.println("/* prototype: " + detail + " */");
					writer.println(c);
				} finally {
					writer.close();
				}
				index.println(va + "\t" + name + "\t" + file.getName() + "\t" + outcome + "\t"
					+ detail.replace('\n', ' ').replace('\t', ' '));
			}
		} finally {
			index.close();
			decompiler.dispose();
		}

		println("ExportDecompilation: " + exported + " ok, " + failed + " failed, " + skipped
			+ " external skipped -> " + outDir);
	}

	private void exportStrings(File outDir) throws Exception {
		// StringSearcher is the searcher CombinedStringSearcher builds for the
		// string table, minus its word-model scoring — that scoring needs a
		// trigram model the headless analyzer never initializes (NGramUtils
		// throws a NullPointerException without it).
		StringSearcher searcher = new StringSearcher(currentProgram, MIN_STRING_LENGTH, 1, true, true);
		List<FoundString> strings = new ArrayList<>();
		searcher.search(currentProgram.getMemory().getLoadedAndInitializedAddressSet(),
			strings::add, true, monitor);
		Collections.sort(strings);

		PrintWriter writer = new PrintWriter(new File(outDir, "strings.tsv"), "UTF-8");
		try {
			writer.println("# va\ttype\ttext");
			for (FoundString found : strings) {
				String value;
				try {
					value = found.getString(currentProgram.getMemory());
				} catch (Exception e) {
					continue;
				}
				writer.println(found.getAddress() + "\t" + found.getDataType().getName() + "\t"
					+ escape(value));
			}
		} finally {
			writer.close();
		}

		println("ExportDecompilation: " + strings.size() + " strings -> " + outDir + "/strings.tsv");
	}

	private static String sanitize(String name) {
		String sanitized = name.replaceAll("[^A-Za-z0-9_.$-]", "_");
		if (sanitized.length() > MAX_NAME_LENGTH) {
			sanitized = sanitized.substring(0, MAX_NAME_LENGTH);
		}
		return sanitized;
	}

	private static String escape(String value) {
		return value.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n")
			.replace("\r", "\\r");
	}
}
