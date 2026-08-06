from django.conf import settings
from django.db import models


class Team(models.Model):
    name = models.CharField(max_length=64)


class Author(models.Model):
    email = models.EmailField()
    team = models.ForeignKey(Team, on_delete=models.CASCADE)


class Profile(models.Model):
    author = models.OneToOneField(
        Author,
        on_delete=models.CASCADE,
        related_name="profile",
    )
    biography = models.TextField()


class Tag(models.Model):
    name = models.CharField(max_length=64)


class Blog(models.Model):
    title = models.CharField(max_length=255)
    author = models.ForeignKey(
        Author,
        on_delete=models.CASCADE,
        related_name="blogs",
        related_query_name="authored_blogs",
    )
    tags = models.ManyToManyField(Tag, related_name="blogs")


class Route(models.Model):
    installer = models.ForeignKey(
        settings.AUTH_USER_MODEL,
        on_delete=models.CASCADE,
        related_name="routes",
    )
