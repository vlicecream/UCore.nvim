-- 防止重复加载
if vim.g.loaded_ucore == 1 then
    return
end
vim.g.loaded_ucore = 1

-- 创建一个用户命令 :UCoreHello
-- 当你输入这个命令时，它会调用我们上面写的那个函数
vim.api.nvim_create_user_command('UCoreHello', function()
    require('ucore').say_hello()
end, {})