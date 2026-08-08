from .models import Author, Blog, Route


Blog.objects.filter(author__te)
Route.objects.filter(installer__time)
Blog.objects.select_related("author__te")
Author.objects.prefetch_related("bl")
