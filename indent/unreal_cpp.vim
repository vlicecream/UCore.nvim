" Reuse Neovim's C++ indentation for Unreal C++ buffers.
" 复用 Neovim 的 C++ 缩进，让 ft=unreal_cpp 拥有正常代码块回车缩进。
runtime! indent/cpp.vim

if !exists("b:did_indent")
  let b:did_indent = 1
  setlocal autoindent
  setlocal cindent
endif
