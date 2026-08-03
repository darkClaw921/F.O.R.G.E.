# UploadPart

Одна часть multipart-запроса к upload-серверу VK (plugins/vk/src/api/mod.rs): field (photo | file1..file5 | file — имена диктует VK), file_name, mime, size и готовое reqwest::Body. Отдельная структура, а не пять аргументов: у альбома частей до пяти, и позиционный вызов на пятнадцать значений нечитаем. Debug печатает всё, кроме тела (оно поток). Потребитель — VkClient::upload_multipart_parts.
