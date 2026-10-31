// Compiler-owned closure diagnostic. This is not an effects proof.
// Build against the same Clang major that produced the serialized AST.
#include "clang/AST/ASTContext.h"
#include "clang/AST/DeclCXX.h"
#include "clang/AST/ExprCXX.h"
#include "clang/AST/RecursiveASTVisitor.h"
#include "clang/Frontend/ASTUnit.h"
#include "clang/Frontend/CompilerInstance.h"
#include "clang/Index/USRGeneration.h"
#include "clang/Serialization/PCHContainerOperations.h"
#include "clang/Serialization/ASTReader.h"
#include "clang/Serialization/ModuleManager.h"
#include "llvm/Support/JSON.h"
#include "llvm/ADT/StringExtras.h"
#include "llvm/Support/SHA256.h"
#include "llvm/Support/xxhash.h"
#include "llvm/Support/raw_ostream.h"
#include <cstdlib>
#include <set>
#include <string>
#include <vector>

using namespace clang;

namespace {
constexpr size_t MaxDeclarations = 20000;
constexpr size_t MaxVisits = 1000000;

std::string identity(const void *pointer) {
  std::string text;
  llvm::raw_string_ostream stream(text);
  stream << pointer;
  return text;
}

class Closure : public RecursiveASTVisitor<Closure> {
public:
  ASTContext &context;
  explicit Closure(ASTContext &context) : context(context) {}
  std::vector<Decl *> pending;
  std::set<Decl *> seen;
  std::set<FunctionDecl *> callable;
  std::set<std::string> failures;
  size_t visits = 0;

  bool shouldVisitImplicitCode() const { return true; }
  bool shouldVisitTemplateInstantiations() const { return true; }

  bool TraverseDecl(Decl *decl) {
    // Retain whole record JSON (including unused methods), but walk effects
    // only for compiler-resolved callable dependencies. A template pattern is
    // not the instantiated body of its specialization.
    if (auto *function = dyn_cast_or_null<FunctionDecl>(decl))
      if (!callable.count(function->getCanonicalDecl()))
        return true;
    return RecursiveASTVisitor<Closure>::TraverseDecl(decl);
  }

  void addCallable(FunctionDecl *decl) {
    if (!decl) {
      failures.insert("unresolved callable");
      return;
    }
    if (callable.insert(decl->getCanonicalDecl()).second && seen.count(decl))
      pending.push_back(decl);
    add(decl);
  }

  void add(Decl *decl) {
    if (!decl) {
      failures.insert("unresolved declaration");
      return;
    }
    for (auto *redecl : decl->redecls()) {
      if (seen.insert(redecl).second) {
        if (seen.size() > MaxDeclarations) {
          llvm::errs() << "declaration limit\n";
          std::exit(2);
        }
        pending.push_back(redecl);
      }
    }
  }

  bool VisitDecl(Decl *) { return tick(); }
  bool VisitStmt(Stmt *) { return tick(); }
  bool VisitType(Type *) { return tick(); }
  bool tick() {
    if (++visits > MaxVisits) {
      llvm::errs() << "traversal limit\n";
      std::exit(2);
    }
    return true;
  }
  bool VisitDeclRefExpr(DeclRefExpr *expr) {
    if (auto *function = dyn_cast<FunctionDecl>(expr->getDecl()))
      addCallable(function);
    else
      add(expr->getDecl());
    return true;
  }
  bool VisitMemberExpr(MemberExpr *expr) {
    add(expr->getMemberDecl());
    return true;
  }
  bool VisitCallExpr(CallExpr *expr) {
    auto *callee = expr->getDirectCallee();
    if (!callee)
      failures.insert("indirect call at " +
                      expr->getExprLoc().printToString(context.getSourceManager()));
    else {
      // A statically named virtual method is not the complete runtime callee
      // set. Conservatively reject even potentially devirtualizable cases.
      if (auto *method = dyn_cast<CXXMethodDecl>(callee))
        if (method->isVirtual())
          failures.insert("unresolved virtual dispatch: " +
                          method->getQualifiedNameAsString());
      addCallable(callee);
      if (!callee->hasBody())
        failures.insert("called declaration has no body: " +
                        callee->getQualifiedNameAsString());
    }
    return true;
  }
  bool VisitCXXConstructExpr(CXXConstructExpr *expr) {
    addCallable(expr->getConstructor());
    if (!expr->getConstructor()->hasBody())
      failures.insert("constructor has no body: " +
                      expr->getConstructor()->getQualifiedNameAsString());
    return true;
  }
  bool VisitCXXBindTemporaryExpr(CXXBindTemporaryExpr *expr) {
    auto *destructor = expr->getTemporary()->getDestructor();
    addCallable(const_cast<CXXDestructorDecl *>(destructor));
    if (!destructor->hasBody())
      failures.insert("temporary destructor has no body");
    return true;
  }
  bool VisitVarDecl(VarDecl *decl) {
    if (auto *record = context.getBaseElementType(decl->getType())->getAsCXXRecordDecl()) {
      if (auto *destructor = record->getDestructor()) {
        addCallable(destructor);
        if (!destructor->hasBody() && !destructor->isTrivial())
          failures.insert("automatic destructor has no body");
      }
    }
    return true;
  }
  bool VisitTagType(TagType *type) {
    add(type->getDecl());
    if (auto *definition = type->getDecl()->getDefinition())
      add(definition);
    return true;
  }
  bool VisitTypedefType(TypedefType *type) {
    add(type->getDecl());
    return true;
  }
  bool VisitVariableArrayType(VariableArrayType *) {
    failures.insert("variable length array");
    return true;
  }
  bool VisitExprWithCleanups(ExprWithCleanups *expr) {
    // Non-temporary cleanup objects require an explicit dependency model.
    if (expr->getNumObjects())
      failures.insert("unmodeled cleanup object");
    return true;
  }
};

void roots(DeclContext *context, llvm::StringRef name,
           std::set<FunctionDecl *> &found) {
  for (auto *decl : context->decls()) {
    if (auto *function = dyn_cast<FunctionDecl>(decl)) {
      if (function->getIdentifier() && function->getName() == name &&
          function->isExternC())
        found.insert(function->getCanonicalDecl());
    } else if (isa<NamespaceDecl, LinkageSpecDecl>(decl)) {
      roots(cast<DeclContext>(decl), name, found);
    }
  }
}
} // namespace

int main(int argc, char **argv) {
  if (argc != 3 && argc != 4) {
    llvm::errs() << "usage: metadata_ast_dump SERIALIZED_AST C_EXPORT [CLOSED_VFS]\n";
    return 2;
  }
  auto diagnostics = CompilerInstance::createDiagnostics(
      *llvm::vfs::getRealFileSystem(), new DiagnosticOptions);
  PCHContainerOperations containers;
  auto headerOptions = std::make_shared<HeaderSearchOptions>();
  headerOptions->ValidateASTInputFilesContent = true;
  headerOptions->ModulesValidateSystemHeaders = true;
  headerOptions->ModulesValidateOncePerBuildSession = false;
  headerOptions->ForceCheckCXX20ModulesInputFiles = true;
  llvm::IntrusiveRefCntPtr<llvm::vfs::FileSystem> filesystem =
      llvm::vfs::getRealFileSystem();
  if (argc == 4) {
    auto overlay = llvm::MemoryBuffer::getFile(argv[3]);
    if (!overlay)
      return 2;
    auto closed = llvm::vfs::getVFSFromYAML(std::move(*overlay), nullptr, argv[3]);
    if (!closed)
      return 2;
    filesystem = closed.release();
  }
  auto ast = ASTUnit::LoadFromASTFile(
      argv[1], containers.getRawReader(), ASTUnit::LoadEverything, diagnostics,
      FileSystemOptions(), headerOptions, nullptr, false, CaptureDiagsKind::None,
      false, false, filesystem);
  if (!ast || diagnostics->hasErrorOccurred())
    return 2;
  // ASTUnit's default freshness checks can accept changed content when size
  // and mtime match. Compare every compiler-indexed input hash explicitly.
  // LLVM20's two visitation APIs enumerate the same module input IDs in order;
  // no filename or declaration-name join is used here.
  auto reader = ast->getASTReader();
  if (!reader)
    return 2;
  size_t moduleCount = 0;
  llvm::json::Array serializedInputs;
  for (auto &module : reader->getModuleManager()) {
    if (++moduleCount > 64 || module.InputFilesLoaded.size() > 4096)
      return 2;
    if (!module.Buffer || module.Buffer->getBufferSize() > 1024ULL * 1024 * 1024)
      return 2;
    llvm::SHA256 moduleHash;
    moduleHash.update(module.Buffer->getBuffer());
    serializedInputs.push_back(llvm::json::Object{
        {"path", module.FileName},
        {"bytes", static_cast<int64_t>(module.Buffer->getBufferSize())},
        {"sha256", llvm::toHex(moduleHash.final(), true)}});
    std::vector<serialization::InputFileInfo> infos;
    reader->visitInputFileInfos(module, true, [&](const auto &info, bool) {
      infos.push_back(info);
    });
    size_t ordinal = 0;
    bool mismatch = false;
    uint64_t checkedBytes = 0;
    reader->visitInputFiles(module, true, true, [&](const auto &input, bool) {
      if (ordinal >= infos.size()) {
        mismatch = true;
        return;
      }
      const auto &info = infos[ordinal++];
      auto file = input.getFile();
      if (!file || input.isOverridden() || input.isOutOfDate() ||
          info.Overridden || info.Transient || !info.ContentHash) {
        mismatch = true;
        return;
      }
      auto buffer = ast->getFileManager().getBufferForFile(*file);
      if (!buffer || (*buffer)->getBufferSize() > 256 * 1024 * 1024 - checkedBytes) {
        mismatch = true;
        return;
      }
      checkedBytes += (*buffer)->getBufferSize();
      if (llvm::xxh3_64bits((*buffer)->getBuffer()) != info.ContentHash)
        mismatch = true;
    });
    if (mismatch || ordinal != infos.size()) {
      llvm::errs() << "serialized input content mismatch or unavailable hash\n";
      return 2;
    }
  }
  std::set<FunctionDecl *> found;
  roots(ast->getASTContext().getTranslationUnitDecl(), argv[2], found);
  if (found.size() != 1) {
    llvm::errs() << "root is not one canonical C export\n";
    return 2;
  }
  Closure closure(ast->getASTContext());
  if (!(*found.begin())->hasBody())
    closure.failures.insert("root declaration has no body");
  closure.addCallable(*found.begin());
  for (size_t index = 0; index < closure.pending.size(); ++index)
    if (!closure.TraverseDecl(closure.pending[index]))
      return 2;
  // Bind the bytes supplied by this loaded SourceManager, not filenames from
  // an imported receipt. This is an input inventory, not proof that a stale
  // serialized AST was generated from these same bytes.
  auto &sources = ast->getSourceManager();
  // Serialized source locations are lazy. Materialize the bounded table
  // before enumerating FileInfos, or even the main input can be omitted.
  if (sources.loaded_sloc_entry_size() > MaxVisits)
    return 2;
  for (unsigned index = 0; index < sources.loaded_sloc_entry_size(); ++index) {
    bool invalid = false;
    sources.getLoadedSLocEntry(index, &invalid);
    if (invalid)
      return 2;
  }
  std::vector<FileEntryRef> files;
  for (auto iter = sources.fileinfo_begin(); iter != sources.fileinfo_end(); ++iter)
    files.push_back(iter->first);
  if (files.size() > 4096)
    return 2;
  llvm::json::Array inputs;
  uint64_t inputBytes = 0;
  for (auto file : files) {
    auto fid = sources.translateFile(file);
    bool invalid = false;
    auto bytes = sources.getBufferData(fid, &invalid);
    if (invalid || bytes.size() > 256 * 1024 * 1024 - inputBytes)
      return 2;
    inputBytes += bytes.size();
    llvm::SHA256 hash;
    hash.update(bytes);
    inputs.push_back(llvm::json::Object{
        {"path", file.getName()}, {"bytes", static_cast<int64_t>(bytes.size())},
        {"sha256", llvm::toHex(hash.final(), true)}});
  }
  // Each node is dumped by Clang itself in this single ASTContext. Context
  // identity is emitted separately, not reconstructed by qualified names.
  auto &out = llvm::outs();
  out << "{\"schema\":1,\"effects_proven\":false,\"provenance_verified\":false,"
         "\"root\":";
  out << llvm::json::Value(identity(*found.begin()));
  out << ",\"serialized_inputs\":" << llvm::json::Value(std::move(serializedInputs));
  out << ",\"loaded_source_inputs\":" << llvm::json::Value(std::move(inputs));
  out << ",\"declarations\":[";
  bool first = true;
  for (auto *decl : closure.seen) {
    if (!first)
      out << ',';
    first = false;
    llvm::SmallString<128> usr;
    clang::index::generateUSRForDecl(decl, usr);
    out << "{\"canonical\":"
        << llvm::json::Value(identity(decl->getCanonicalDecl()))
        << ",\"semantic_context\":"
        << llvm::json::Value(identity(Decl::castFromDeclContext(decl->getDeclContext())))
        << ",\"lexical_context\":"
        << llvm::json::Value(identity(Decl::castFromDeclContext(decl->getLexicalDeclContext())))
        << ",\"usr\":" << llvm::json::Value(usr.str()) << ",\"node\":";
    decl->dump(out, true, ADOF_JSON);
    out << '}';
  }
  out << "],\"callable\":[";
  first = true;
  for (auto *function : closure.callable) {
    if (!first)
      out << ',';
    first = false;
    out << llvm::json::Value(identity(function));
  }
  out << "],\"unresolved\":[";
  first = true;
  for (const auto &failure : closure.failures) {
    if (!first)
      out << ',';
    first = false;
    out << llvm::json::Value(failure);
  }
  out << "]}\n";
  // Until the driver binds bytes/profile/includes, this remains diagnostic.
  return closure.failures.empty() ? 0 : 1;
}
