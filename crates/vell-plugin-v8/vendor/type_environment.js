(() => {
  "use strict";

  const generatedFile = "/commands.generated.d.ts";
  const files = new Map();
  const versions = new Map();
  const commands = new Map();
  let projectVersion = 0;

  function canonical(fileName) {
    return fileName.replaceAll("\\", "/");
  }

  function updateSource(fileName, source) {
    const name = canonical(fileName);
    if (files.get(name) === source) return null;
    files.set(name, source);
    versions.set(name, (versions.get(name) ?? 0) + 1);
    projectVersion += 1;
    return null;
  }

  function resolvedModule(moduleName, containingFile) {
    if (!moduleName.startsWith("./") && !moduleName.startsWith("../")) {
      return undefined;
    }
    const directory = canonical(containingFile).replace(/\/[^/]*$/, "");
    const parts = `${directory}/${moduleName}`.split("/");
    const normalized = [];
    for (const part of parts) {
      if (!part || part === ".") continue;
      if (part === "..") normalized.pop();
      else normalized.push(part);
    }
    const prefix = directory.match(/^\/+/)?.[0] ?? "";
    const base = `${prefix}${normalized.join("/")}`;
    const candidates = [
      base,
      `${base}.ts`,
      `${base}.tsx`,
      `${base}.d.ts`,
      `${base}/index.ts`,
    ];
    const resolvedFileName = candidates.find((candidate) => files.has(candidate));
    if (!resolvedFileName) return undefined;
    const extension = resolvedFileName.endsWith(".tsx")
      ? ts.Extension.Tsx
      : resolvedFileName.endsWith(".d.ts")
        ? ts.Extension.Dts
        : ts.Extension.Ts;
    return { resolvedFileName, extension };
  }

  const host = {
    getCompilationSettings() {
      return {
        allowImportingTsExtensions: true,
        module: ts.ModuleKind.ES2022,
        noEmit: true,
        noLib: true,
        skipLibCheck: true,
        strict: true,
        target: ts.ScriptTarget.ES2022,
      };
    },
    getCurrentDirectory: () => "/",
    getDefaultLibFileName: () => "/lib.es5.d.ts",
    getNewLine: () => "\n",
    getProjectVersion: () => String(projectVersion),
    getScriptFileNames: () => [...files.keys()],
    getScriptSnapshot(fileName) {
      const source = files.get(canonical(fileName));
      return source === undefined ? undefined : ts.ScriptSnapshot.fromString(source);
    },
    getScriptVersion: (fileName) => String(versions.get(canonical(fileName)) ?? 0),
    readFile: (fileName) => files.get(canonical(fileName)),
    fileExists: (fileName) => files.has(canonical(fileName)),
    directoryExists: () => true,
    getDirectories: () => [],
    readDirectory: () => [],
    useCaseSensitiveFileNames: () => true,
    resolveModuleNames(moduleNames, containingFile) {
      return moduleNames.map((name) => resolvedModule(name, containingFile));
    },
  };

  const service = ts.createLanguageService(
    host,
    ts.createDocumentRegistry(true, "/"),
  );

  function isRegisterCall(node) {
    if (!ts.isCallExpression(node) || !ts.isPropertyAccessExpression(node.expression)) {
      return false;
    }
    const register = node.expression;
    if (register.name.text !== "register" ||
        !ts.isPropertyAccessExpression(register.expression)) {
      return false;
    }
    const namespace = register.expression;
    return namespace.name.text === "commands" &&
      ts.isIdentifier(namespace.expression) &&
      namespace.expression.text === "editor";
  }

  function findRegisterCall(sourceFile, line, column) {
    const candidates = [];
    function visit(node) {
      if (isRegisterCall(node)) {
        const start = sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile));
        const end = sourceFile.getLineAndCharacterOfPosition(node.getEnd());
        const oneBasedLine = start.line + 1;
        if (line >= oneBasedLine && line <= end.line + 1) {
          const score = Math.abs(oneBasedLine - line) * 100000 +
            Math.abs(start.character + 1 - column);
          candidates.push({ node, score });
        }
      }
      ts.forEachChild(node, visit);
    }
    visit(sourceFile);
    candidates.sort((left, right) => left.score - right.score);
    return candidates[0]?.node;
  }

  function literalCommandId(node) {
    return ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node)
      ? node.text
      : undefined;
  }

  function callbackForRegistration(call, id) {
    if (call.arguments.length === 2) {
      return literalCommandId(call.arguments[0]) === id
        ? call.arguments[1]
        : undefined;
    }
    if (call.arguments.length !== 1) return undefined;
    const callback = call.arguments[0];
    if (ts.isIdentifier(callback)) return callback;
    if (ts.isFunctionExpression(callback) && callback.name) return callback;
    return undefined;
  }

  function inferSignature(registration) {
    if (!registration.source || !registration.line || !registration.column) {
      return "(...arguments: unknown[]) => unknown";
    }
    const program = service.getProgram();
    const sourceFile = program?.getSourceFile(canonical(registration.source));
    if (!program || !sourceFile) {
      return "(...arguments: unknown[]) => unknown";
    }
    const call = findRegisterCall(
      sourceFile,
      registration.line,
      registration.column,
    );
    const callback = call && callbackForRegistration(call, registration.id);
    if (!callback) return "(...arguments: unknown[]) => unknown";
    const checker = program.getTypeChecker();
    const signatures = checker.getSignaturesOfType(
      checker.getTypeAtLocation(callback),
      ts.SignatureKind.Call,
    );
    if (signatures.length === 0) {
      return "(...arguments: unknown[]) => unknown";
    }
    return formatSignatures(signatures, checker, callback);
  }

  function formatSignatures(signatures, checker, location) {
    const flags = ts.TypeFormatFlags.NoTruncation |
      ts.TypeFormatFlags.UseStructuralFallback |
      ts.TypeFormatFlags.WriteArrowStyleSignature;
    return signatures
      .map((signature) => checker.signatureToString(
        signature,
        location,
        flags,
        ts.SignatureKind.Call,
      ))
      .map((signature) => `(${signature})`)
      .join(" & ");
  }

  function initializeCommands() {
    const program = service.getProgram();
    const sourceFile = program?.getSourceFile(generatedFile);
    if (!program || !sourceFile) {
      throw new Error("native command declarations missing");
    }
    const checker = program.getTypeChecker();
    const seeds = sourceFile.statements.find((statement) =>
      ts.isInterfaceDeclaration(statement) &&
      statement.name.text === "EditorCommandSeeds"
    ) ?? sourceFile.statements.find((statement) =>
      ts.isInterfaceDeclaration(statement) &&
      statement.name.text === "EditorCommands"
    );
    if (!seeds) {
      throw new Error("native command seed interface missing");
    }
    for (const member of seeds.members) {
      if (!ts.isMethodSignature(member) && !ts.isPropertySignature(member)) {
        continue;
      }
      const name = member.name &&
        (ts.isIdentifier(member.name) || ts.isStringLiteral(member.name))
        ? member.name.text
        : undefined;
      if (!name) continue;
      const signature = ts.isMethodSignature(member)
        ? checker.getSignatureFromDeclaration(member)
        : undefined;
      const signatures = signature
        ? [signature]
        : checker.getSignaturesOfType(
          checker.getTypeAtLocation(member),
          ts.SignatureKind.Call,
        );
      if (signatures.length > 0) {
        commands.set(name, formatSignatures(signatures, checker, member));
      }
    }
    return null;
  }

  function commandTree() {
    const root = Object.create(null);
    for (const [id, signature] of commands) {
      let node = root;
      for (const segment of id.split(".")) {
        node.children ??= Object.create(null);
        node.children[segment] ??= Object.create(null);
        node = node.children[segment];
      }
      node.signature = signature;
    }
    return root;
  }

  function renderNode(node, indent) {
    const children = Object.entries(node.children ?? {});
    if (children.length === 0) return node.signature ?? "never";
    const body = children
      .map(([name, child]) =>
        `${" ".repeat(indent + 2)}readonly ${JSON.stringify(name)}: ${renderNode(child, indent + 2)};`)
      .join("\n");
    const namespace = `{\n${body}\n${" ".repeat(indent)}}`;
    return node.signature ? `${node.signature} & ${namespace}` : namespace;
  }

  function rebuildDeclarations() {
    const roots = Object.entries(commandTree().children ?? {});
    const members = roots
      .map(([name, node]) =>
        `  readonly ${JSON.stringify(name)}: ${renderNode(node, 2)};`)
      .join("\n");
    const globals = roots
      .filter(([name]) => isBindingName(name))
      .map(([name, node]) => `declare const ${name}: ${renderNode(node, 0)};`)
      .join("\n");
    updateSource(
      generatedFile,
      `interface EditorCommands {\n${members}\n}\n${globals}\n`,
    );
  }

  function isBindingName(name) {
    const source = ts.createSourceFile(
      "binding.ts",
      `declare const ${name}: unknown;`,
      ts.ScriptTarget.Latest,
      false,
      ts.ScriptKind.TS,
    );
    return source.parseDiagnostics.length === 0;
  }

  function publishRegistrations(registrations) {
    for (const registration of registrations) {
      commands.set(registration.id, inferSignature(registration));
      rebuildDeclarations();
    }
    return files.get(generatedFile);
  }

  function diagnosticText(diagnostic) {
    const message = ts.flattenDiagnosticMessageText(diagnostic.messageText, "\n");
    if (!diagnostic.file || diagnostic.start === undefined) return message;
    const position = diagnostic.file.getLineAndCharacterOfPosition(diagnostic.start);
    return `${diagnostic.file.fileName}:${position.line + 1}:${position.character + 1}: ${message}`;
  }

  function diagnostics(fileName) {
    const name = canonical(fileName);
    return [
      ...service.getSyntacticDiagnostics(name),
      ...service.getSemanticDiagnostics(name),
    ].map(diagnosticText);
  }

  globalThis.__vellTypeEnvironment = {
    diagnostics,
    generatedDeclarations: () => files.get(generatedFile),
    initializeCommands,
    publishRegistrations,
    updateSource,
    version: () => ts.version,
  };
})();
